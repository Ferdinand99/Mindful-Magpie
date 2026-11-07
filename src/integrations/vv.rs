//! The `vv` kind («Vigilant Vole»): the uptime monitor. One call to
//! `GET /api/v1/summary?days=30` gives the monitor counts, the mean uptime
//! and what is down.
//!
//! An older Vigilant Vole (before 0.17.0) has no `/summary`: it answers 404.
//! The kind then asks `GET /api/v1/health`, which has the counts but no uptime
//! figures, and remembers for a few minutes (see [`api::OLD_FOR`]) that this
//! address is old, so it does not ask `/summary` again at every poll. Saving or
//! testing the service makes it ask again at once.
//!
//! The service's secret is a read token, sent as `Authorization: Bearer`.
//! Monitor names and messages come from outside: they go only into the text
//! value `first_down` and the detail blocks (cut down by `Block::bounded`),
//! never into a need. A field that is missing or odd costs only its own
//! metric.
//!
//! **Details.** A summary that lists `down` gives the list «Down now» (name,
//! message, how long). The kind also remembers, in memory per address, which
//! monitors were down at the last fetch: one that has left the list since is
//! «Recently recovered» for 24 hours. Saving or testing the service forgets
//! that memory (see [`api::on_forget`]). An older server whose summary has no
//! `down` list gets neither block.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jiff::tz::TimeZone;
use jiff::{Timestamp, civil};
use serde_json::Value as Json;

use super::api::{self, OldAddresses, Reply};
use super::block::{Block, ListItem, MAX_ITEMS, TEXT_CHARS};
use super::{
    BoxFuture, FetchCtx, FetchError, Field, FieldKind, Format, Integration, Level, MetricDef, Need,
    Snapshot, Status, Value,
};

/// The only instance.
pub static VV: VvKind = VvKind;

/// See the module.
pub struct VvKind;

/// The number of days the uptime figure covers.
const DAYS: &str = "30";
/// Longest `first_down` text, in characters.
const NAME_CHARS: usize = 80;

/// The key of the need «some monitors are down».
pub const DOWN_NEED: &str = "vv:down";

/// The addresses whose `/summary` answered 404.
static OLD: OldAddresses = OldAddresses::new();

/// What is remembered per address (the key is [`api::service_prefix`]).
static MEMORY: Mutex<VecDeque<(String, Memory)>> = Mutex::new(VecDeque::new());

/// Most addresses the kind remembers; past that the one used longest ago goes.
pub const MEMORY_MAX: usize = 64;
/// How long a monitor that came back is listed.
const RECOVERED_FOR: Duration = Duration::from_secs(24 * 60 * 60);
/// Most recovered monitors remembered per address.
const RECOVERED_MAX: usize = 16;
/// Most monitors remembered as down per address (the summary lists 25 at most).
const DOWN_MAX: usize = 100;

static FIELDS: [Field; 1] = [Field {
    key: "secret",
    label_key: "field.vv_secret",
    help_key: "field.vv_secret_help",
    placeholder: "vv_…",
    kind: FieldKind::Text,
    required: true,
    secret: true,
}];

static METRICS: [MetricDef; 6] = [
    MetricDef {
        id: "up",
        label_key: "metric.vv_up",
        format: Format::Number,
    },
    MetricDef {
        id: "down",
        label_key: "metric.vv_down",
        format: Format::Number,
    },
    MetricDef {
        id: "uptime_30d",
        label_key: "metric.vv_uptime_30d",
        format: Format::Percent,
    },
    MetricDef {
        id: "uptime_24h",
        label_key: "metric.vv_uptime_24h",
        format: Format::Percent,
    },
    MetricDef {
        id: "paused",
        label_key: "metric.vv_paused",
        format: Format::Number,
    },
    MetricDef {
        id: "first_down",
        label_key: "metric.vv_first_down",
        format: Format::Text,
    },
];

/// What is read of an answer: the counts, the uptime figures and the name of
/// the monitor that has been down longest (the last two from a summary only).
/// Each is missing when the answer does not have it in a usable form.
#[derive(Debug, Default, PartialEq)]
struct Reading {
    up: Option<f64>,
    down: Option<f64>,
    pending: Option<f64>,
    unknown: Option<f64>,
    paused: Option<f64>,
    uptime_30d: Option<f64>,
    uptime_24h: Option<f64>,
    first_down: Option<String>,
    /// The monitors that are down, as a summary lists them; `None` when the
    /// answer has no list (an older server, or `down` is null).
    down_list: Option<Vec<Down>>,
}

/// A monitor in the summary's `down` list. The texts are cleaned lines.
#[derive(Debug, Clone, PartialEq)]
struct Down {
    id: i64,
    name: String,
    message: Option<String>,
    since: Option<Timestamp>,
}

/// The counts in `monitors`, each as `count` reads it.
fn counts(monitors: &Json, count: impl Fn(Option<&Json>) -> Option<f64>) -> Reading {
    Reading {
        up: count(monitors.get("up")),
        down: count(monitors.get("down")),
        pending: count(monitors.get("pending")),
        unknown: count(monitors.get("unknown")),
        paused: count(monitors.get("paused")),
        ..Reading::default()
    }
}

/// The reading of a `/summary` answer. Without `monitors` there is nothing
/// to show: [`FetchError::Invalid`].
fn summary(doc: &Json) -> Result<Reading, FetchError> {
    let monitors = doc
        .get("monitors")
        .filter(|m| m.is_object())
        .ok_or(FetchError::Invalid)?;
    Ok(Reading {
        // `uptime` and `down` may be null, or missing.
        uptime_30d: api::percent(doc.pointer("/uptime/pct")),
        uptime_24h: api::percent(doc.pointer("/uptime/pct_24h")),
        first_down: doc
            .get("down")
            .and_then(Json::as_array)
            .and_then(|list| list.iter().find_map(|d| d.get("name")?.as_str()))
            .map(|name| api::clean_text(name, NAME_CHARS))
            .filter(|name| !name.is_empty()),
        down_list: doc
            .get("down")
            .and_then(Json::as_array)
            .map(|list| list.iter().filter_map(down_entry).collect()),
        ..counts(monitors, api::count)
    })
}

/// One entry of `down`: it needs a number and a name; its message and time
/// are optional.
fn down_entry(entry: &Json) -> Option<Down> {
    let id = entry.get("id")?.as_i64()?;
    let name = api::clean_text(entry.get("name")?.as_str()?, TEXT_CHARS);
    if name.is_empty() {
        return None;
    }
    Some(Down {
        id,
        name,
        message: entry
            .get("message")
            .and_then(Json::as_str)
            .map(|m| api::clean_text(m, TEXT_CHARS))
            .filter(|m| !m.is_empty()),
        since: entry
            .get("since")
            .and_then(Json::as_str)
            .and_then(parse_time),
    })
}

/// A time as the summary writes it (RFC 3339), or as `YYYY-MM-DD HH:MM:SS` in UTC.
pub(super) fn parse_time(text: &str) -> Option<Timestamp> {
    let text = text.trim();
    if let Ok(at) = text.parse::<Timestamp>() {
        return Some(at);
    }
    let at: civil::DateTime = text.replace(' ', "T").parse().ok()?;
    Some(at.to_zoned(TimeZone::UTC).ok()?.timestamp())
}

/// How long ago `then` was, as one short text that reads the same in every
/// language: `45 s`, `12 min`, `3 h`, `2 d`. Blocks are stored once per
/// service, not per user, so no language goes in. (The age is as old as the
/// reading, up to one interval.)
pub(super) fn age_text(then: Timestamp, now: Timestamp) -> String {
    let secs = now.as_second().saturating_sub(then.as_second()).max(0);
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86399 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86400),
    }
}

/// The reading of a `/health` answer (an older server): the counts only.
fn health(doc: &Json) -> Result<Reading, FetchError> {
    let by_status = doc
        .pointer("/monitors/by_status")
        .filter(|m| m.is_object())
        .ok_or(FetchError::Invalid)?;
    // A status that is not listed has no monitors.
    Ok(counts(by_status, |v| Some(api::count(v).unwrap_or(0.0))))
}

fn snapshot(reading: Reading) -> Snapshot {
    let down = reading.down.unwrap_or(0.0);
    let status = if down > 0.0 {
        Status::Bad
    } else if reading.pending.unwrap_or(0.0) + reading.unknown.unwrap_or(0.0) > 0.0 {
        Status::Warn
    } else {
        Status::Ok
    };
    let mut snapshot = Snapshot::new(status);
    let mut values = BTreeMap::new();
    for (id, number) in [
        ("up", reading.up),
        ("down", reading.down),
        ("paused", reading.paused),
        ("uptime_30d", reading.uptime_30d),
        ("uptime_24h", reading.uptime_24h),
    ] {
        if let Some(number) = number {
            values.insert(id.to_string(), Value::num(number));
        }
    }
    if let Some(name) = reading.first_down {
        values.insert("first_down".to_string(), Value::text(name));
    }
    snapshot.values = values;
    if down > 0.0 {
        snapshot.needs.push(down_need(down));
    }
    snapshot
}

/// The need «n monitors are down». Its text takes only the count (the card
/// shows the service's name next to it); no monitor name goes in.
fn down_need(down: f64) -> Need {
    Need {
        key: DOWN_NEED.to_string(),
        level: Level::Bad,
        label_key: "need.vv_down".to_string(),
        params: BTreeMap::from([("count".to_string(), format!("{down:.0}"))]),
        score: down,
        since: None,
    }
}

// ---------------------------------------------------------------------------
// Details

/// What is remembered of an address between fetches.
#[derive(Debug, Clone, Default)]
struct Memory {
    /// The monitors that were down at the last fetch.
    down: Vec<(i64, String)>,
    /// The monitors that left the list since, newest first.
    recovered: Vec<Recovered>,
}

#[derive(Debug, Clone, PartialEq)]
struct Recovered {
    id: i64,
    name: String,
    at: Timestamp,
}

impl Memory {
    /// Takes in a fetch at `now` that listed `down`. A monitor that was down
    /// at the last fetch and is not now is recovered; one that is down again is
    /// not; one older than [`RECOVERED_FOR`] is forgotten. When the list was
    /// cut short (more monitors are down than it names) a missing one may only
    /// be cut off, so nothing counts as recovered then.
    fn track(&mut self, now: Timestamp, down: &[Down], cut_short: bool) {
        let still = |id: i64| down.iter().any(|d| d.id == id);
        self.recovered.retain(|r| {
            !still(r.id)
                && now
                    .as_second()
                    .saturating_sub(r.at.as_second())
                    .unsigned_abs()
                    < RECOVERED_FOR.as_secs()
        });
        if !cut_short {
            for (id, name) in &self.down {
                if !still(*id) {
                    self.recovered.retain(|r| r.id != *id);
                    self.recovered.insert(
                        0,
                        Recovered {
                            id: *id,
                            name: name.clone(),
                            at: now,
                        },
                    );
                }
            }
            self.down.clear();
        }
        for d in down {
            if !self.down.iter().any(|(id, _)| *id == d.id) {
                self.down.push((d.id, d.name.clone()));
            }
        }
        self.down.truncate(DOWN_MAX);
        self.recovered.truncate(RECOVERED_MAX);
    }
}

/// Runs `f` on the memory of `key` (made if it is not there), which then
/// counts as the most recently used.
fn remember<R>(key: &str, f: impl FnOnce(&mut Memory) -> R) -> R {
    // Saving or testing the service forgets what is remembered of it.
    api::on_forget(forget);
    // A panic elsewhere cannot leave the list in a state that matters.
    let mut list = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    let mut entry = match list.iter().position(|(k, _)| k == key) {
        Some(at) => list.remove(at).expect("the position is in the list"),
        None => {
            while list.len() >= MEMORY_MAX {
                list.pop_front();
            }
            (key.to_string(), Memory::default())
        }
    };
    let out = f(&mut entry.1);
    list.push_back(entry);
    out
}

/// Forgets the memory of the service at `prefix` (see [`api::service_prefix`]).
fn forget(prefix: &str) {
    MEMORY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(key, _)| key != prefix);
}

/// The blocks of a summary: «Down now» and «Recently recovered». None without
/// a `down` list in the answer.
fn details(key: &str, reading: &Reading, now: Timestamp) -> Vec<Block> {
    let Some(down) = &reading.down_list else {
        return Vec::new();
    };
    let cut_short = reading.down.is_some_and(|n| n > down.len() as f64);
    let recovered = remember(key, |memory| {
        memory.track(now, down, cut_short);
        memory.recovered.clone()
    });
    vec![
        Block::List {
            title_key: "vv_down_now".to_string(),
            items: down
                .iter()
                .take(MAX_ITEMS)
                .map(|d| ListItem {
                    text: d.name.clone(),
                    sub: d.message.clone(),
                    status: Some(Status::Bad),
                    right: d.since.map(|since| age_text(since, now)),
                    href: None,
                })
                .collect(),
        },
        Block::List {
            title_key: "vv_recovered".to_string(),
            items: recovered
                .iter()
                .take(MAX_ITEMS)
                .map(|r| ListItem {
                    text: r.name.clone(),
                    sub: None,
                    status: Some(Status::Ok),
                    right: Some(age_text(r.at, now)),
                    href: None,
                })
                .collect(),
        },
    ]
}

async fn read(ctx: &FetchCtx) -> Result<Snapshot, FetchError> {
    let base = api::base_url(&ctx.settings)?;
    let auth = api::Credential::bearer(ctx.secret.as_deref())?;
    let path = |last| ["api", "v1", last];
    let key = api::endpoint(&base, &path("summary"), None)?.to_string();
    if !OLD.is_old(&key, Instant::now()) {
        let url = api::endpoint(&base, &path("summary"), Some(&format!("days={DAYS}")))?;
        match api::get_json::<Json>(&ctx.http, url, &auth).await? {
            Reply::Found(doc) => {
                OLD.forget(&key);
                let reading = summary(&doc)?;
                let blocks = details(&api::service_prefix(&base), &reading, Timestamp::now());
                return Ok(snapshot(reading).with_details(blocks));
            }
            Reply::NotFound => OLD.mark(&key, Instant::now()),
        }
    }
    // An old server: the counts of `/health`, and no uptime figures. A 404
    // here too is not Vigilant Vole, or older than any this kind knows.
    let url = api::endpoint(&base, &path("health"), None)?;
    let doc: Json = api::get_json(&ctx.http, url, &auth).await?.found()?;
    health(&doc).map(snapshot)
}

impl Integration for VvKind {
    fn kind(&self) -> &'static str {
        "vv"
    }

    fn label_key(&self) -> &'static str {
        "kind.vv"
    }

    fn fields(&self) -> &'static [Field] {
        &FIELDS
    }

    fn metrics(&self) -> &'static [MetricDef] {
        &METRICS
    }

    fn default_interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn min_interval(&self) -> Duration {
        Duration::from_secs(15)
    }

    fn fetch<'a>(&'a self, ctx: &'a FetchCtx) -> BoxFuture<'a, Result<Snapshot, FetchError>> {
        Box::pin(read(ctx))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_summary_with_null_or_odd_fields_keeps_what_is_good() {
        let doc = json!({
            "monitors": { "up": 5, "down": null, "pending": "x", "unknown": -1, "paused": 2 },
            "uptime": null,
            "down": null,
        });
        let reading = summary(&doc).unwrap();
        assert_eq!(reading.up, Some(5.0));
        assert_eq!(reading.down, None);
        assert_eq!(reading.paused, Some(2.0));
        assert_eq!(reading.uptime_30d, None);
        assert_eq!(reading.first_down, None);
        let snapshot = snapshot(reading);
        assert_eq!(snapshot.status, Status::Ok);
        let ids: Vec<_> = snapshot.values.keys().map(String::as_str).collect();
        assert_eq!(ids, ["paused", "up"]);
    }

    #[test]
    fn the_uptime_is_clamped_and_a_hostile_name_is_cleaned() {
        let doc = json!({
            "monitors": { "up": 1, "down": 1 },
            "uptime": { "pct": 140.0, "pct_24h": -2.0 },
            "down": [{ "name": null }, { "name": "a\u{202E}b\nc" }],
        });
        let reading = summary(&doc).unwrap();
        assert_eq!(reading.uptime_30d, Some(100.0));
        assert_eq!(reading.uptime_24h, Some(0.0));
        assert_eq!(reading.first_down.as_deref(), Some("ab c"));
    }

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + secs).unwrap()
    }

    fn down(id: i64, name: &str) -> Down {
        Down {
            id,
            name: name.to_string(),
            message: None,
            since: None,
        }
    }

    #[test]
    fn ages_are_short_and_the_same_in_every_language() {
        let now = at(100_000);
        for (secs, text) in [
            (0, "0 s"),
            (59, "59 s"),
            (60, "1 min"),
            (3599, "59 min"),
            (3600, "1 h"),
            (86_399, "23 h"),
            (86_400, "1 d"),
            (5 * 86_400, "5 d"),
            // A time in the future (clocks differ) is not negative.
            (-30, "0 s"),
        ] {
            assert_eq!(age_text(at(100_000 - secs), now), text, "{secs}");
        }
    }

    #[test]
    fn times_are_read_as_rfc_3339_or_utc_text() {
        assert_eq!(
            parse_time("2026-11-07T08:01:00Z"),
            parse_time("2026-11-07 08:01:00")
        );
        assert!(parse_time("2026-11-07T08:01:00Z").is_some());
        assert_eq!(parse_time("yesterday"), None);
    }

    #[test]
    fn a_monitor_that_leaves_the_list_is_recovered_for_a_day() {
        let mut memory = Memory::default();
        memory.track(at(0), &[down(1, "a"), down(2, "b")], false);
        assert!(memory.recovered.is_empty());
        memory.track(at(60), &[down(2, "b")], false);
        assert_eq!(memory.recovered.len(), 1);
        assert_eq!(memory.recovered[0].name, "a");
        assert_eq!(memory.recovered[0].at, at(60));
        // Still there at the next fetch, with its first time.
        memory.track(at(120), &[down(2, "b")], false);
        assert_eq!(memory.recovered[0].at, at(60));
        // Gone after a day.
        let later = 60 + RECOVERED_FOR.as_secs() as i64;
        memory.track(at(later), &[down(2, "b")], false);
        assert!(memory.recovered.is_empty());
    }

    #[test]
    fn a_monitor_that_is_down_again_is_not_recovered() {
        let mut memory = Memory::default();
        memory.track(at(0), &[down(1, "a")], false);
        memory.track(at(60), &[], false);
        assert_eq!(memory.recovered.len(), 1);
        memory.track(at(120), &[down(1, "a")], false);
        assert!(memory.recovered.is_empty());
        // And going away again starts over with a new time.
        memory.track(at(180), &[], false);
        assert_eq!(memory.recovered[0].at, at(180));
    }

    #[test]
    fn a_cut_short_list_recovers_nothing_and_forgets_nothing() {
        let mut memory = Memory::default();
        memory.track(at(0), &[down(1, "a"), down(2, "b")], false);
        memory.track(at(60), &[down(2, "b")], true);
        assert!(memory.recovered.is_empty());
        // `a` is still remembered as down, so it is recovered when the list
        // is whole again and does not have it.
        memory.track(at(120), &[down(2, "b")], false);
        assert_eq!(memory.recovered[0].name, "a");
    }

    #[test]
    fn what_is_remembered_is_bounded() {
        let mut memory = Memory::default();
        let many: Vec<Down> = (0..500).map(|i| down(i, "m")).collect();
        memory.track(at(0), &many, false);
        assert_eq!(memory.down.len(), DOWN_MAX);
        memory.track(at(60), &[], false);
        assert_eq!(memory.recovered.len(), RECOVERED_MAX);
        for n in 0..(MEMORY_MAX + 10) {
            remember(&format!("http://bounded-{n}.test/"), |m| {
                m.down.push((1, "x".into()))
            });
        }
        let held = MEMORY.lock().unwrap().len();
        assert!(held <= MEMORY_MAX, "{held}");
        forget("http://bounded-80.test/");
    }

    #[test]
    fn no_monitors_is_not_a_reading() {
        for doc in [
            json!({}),
            json!({ "monitors": null }),
            json!({ "monitors": 3 }),
        ] {
            assert_eq!(summary(&doc).unwrap_err(), FetchError::Invalid);
        }
        assert_eq!(health(&json!({})).unwrap_err(), FetchError::Invalid);
    }
}
