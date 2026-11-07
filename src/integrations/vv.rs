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
//! Monitor names come from outside: they are shown only as the text value
//! `first_down`, never in a need. A field that is missing or odd costs only
//! its own metric.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::Value as Json;

use super::api::{self, OldAddresses, Reply};
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
        ..counts(monitors, api::count)
    })
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

async fn read(ctx: &FetchCtx) -> Result<Snapshot, FetchError> {
    let base = api::base_url(&ctx.settings)?;
    let auth = api::bearer(ctx.secret.as_deref())?;
    let path = |last| ["api", "v1", last];
    let key = api::endpoint(&base, &path("summary"), None)?.to_string();
    if !OLD.is_old(&key, Instant::now()) {
        let url = api::endpoint(&base, &path("summary"), Some(&format!("days={DAYS}")))?;
        match api::get_json::<Json>(&ctx.http, url, &auth).await? {
            Reply::Found(doc) => {
                OLD.forget(&key);
                return summary(&doc).map(snapshot);
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
