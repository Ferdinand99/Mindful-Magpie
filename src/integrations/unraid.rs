//! The `unraid` kind («Unraid»): the server's own GraphQL API (built in from
//! Unraid 7.2). One `POST /graphql` gives the array, the disks, the parity
//! check, the containers and, for the top bar, the name, CPU and memory.
//!
//! The secret is an API key, sent as the header `x-api-key`. Create it under
//! Settings → Management Access → API Keys with the role VIEWER.
//!
//! GraphQL answers 200 even when something is wrong. An answer with `errors`
//! and no usable `data` is no reading ([`FetchError::Invalid`], or
//! [`FetchError::Auth`] when every error says the key may not read it). An
//! answer with `data` and `errors` (a role that may not read one field, say)
//! gives the metrics that did come, and fewer of them. When the errors are
//! about the array or the containers the card is a warning with the reason;
//! errors about `info` or `metrics` (only the top bar's figures) are silent.
//!
//! **An older schema.** A server that does not know `metrics` or `info`
//! rejects the whole query (HTTP 400, or `GRAPHQL_VALIDATION_FAILED` with no
//! data). The kind then asks again with the core query (array and
//! containers only) and remembers the address for [`api::OLD_FOR`] (a few minutes), so these extras can
//! never break the card; the server row then has no name, CPU or memory.
//!
//! **The disk figure.** `disk_used` is the use of the *fullest* disk (data
//! disks and cache pools), not of the array as a whole: Unraid fills disks
//! one by one, so a single full disk is the thing that needs attention while
//! the array's total still looks roomy. Only the ratio `fsUsed / fsSize` of
//! one disk is used, so it does not matter whether the `fs*` fields are in
//! bytes or in KiB, and they may arrive as numbers or as strings (`BigInt`).
//! When no disk has file system figures (a stopped array) the array's
//! `capacity.kilobytes` ratio is used instead, which is a ratio too.
//!
//! **The server row.** [`server_row`] takes the `data` of an answer and gives
//! the name, CPU, memory and disk figure of the server, for a top bar that
//! shows one row per server. The query asks for `metrics { cpu { percentTotal
//! } memory { percentTotal } }`, which the schema has; whether the role VIEWER
//! may read them is not confirmed, and a denied field is only a missing
//! number.
//!
//! **The details.** [`details`] fills blocks from the same answer: bars for the
//! disks and caches (name, use, size and temperature), the parity check's
//! facts (last check, duration, errors), the stopped containers, and the
//! unread notifications. The names (disks, containers) come from outside and
//! go only into these blocks. What they need beyond the core query (`name`
//! and `temp` of a disk, `duration` of the check, `names` of a container, and
//! `notifications`) is asked only by the full query, so an older Unraid that
//! rejects it still gives the metrics and the facts the core query has, and
//! the card then has fewer blocks. An error about one of these extras is
//! silent (see [`is_extra`]). The unit of the file system figures is read as
//! KiB (what Unraid's `disks.ini` has; the schema says kilobytes) and shown in
//! decimal units like Unraid's own pages; whether the role VIEWER may read
//! `notifications` is not confirmed, and a denied field costs only that block.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::{Value as Json, json};

use super::api::{self, Credential, OldAddresses};
use super::block::{Bar, Block, Fact, ListItem, MAX_ITEMS};
use super::{
    BoxFuture, FetchCtx, FetchError, Field, FieldKind, Format, Integration, Level, MetricDef, Need,
    Snapshot, Status, Value,
};

/// The only instance.
pub static UNRAID: UnraidKind = UnraidKind;

/// See the module.
pub struct UnraidKind;

/// The header that carries the key.
const KEY_HEADER: &str = "x-api-key";
/// The addresses whose server rejected the full query.
static CORE_ONLY: OldAddresses = OldAddresses::new();
/// Disk use, in percent, where the card turns to a warning.
pub const DISK_WARN: f64 = 85.0;
/// Disk use, in percent, where the need turns bad.
pub const DISK_BAD: f64 = 95.0;
/// Longest array state or host name, in characters.
const TEXT_CHARS: usize = 60;

/// The needs.
pub const ARRAY_NEED: &str = "unraid:array";
pub const PARITY_NEED: &str = "unraid:parity_errors";
pub const DISK_NEED: &str = "unraid:disk";

/// What is asked: only what is read below.
const QUERY: &str = "query {
  array {
    state
    capacity { kilobytes { used total } }
    parityCheckStatus { running errors date duration }
    disks { name temp fsSize fsUsed fsFree }
    caches { name temp fsSize fsUsed fsFree }
  }
  docker { containers { state names } }
  info { os { hostname } }
  metrics { cpu { percentTotal } memory { percentTotal } }
  notifications { overview { unread { total alert warning } } }
}";

/// The same without `info`, `metrics` and the fields only the details read,
/// for a server that rejects those.
const CORE_QUERY: &str = "query {
  array {
    state
    capacity { kilobytes { used total } }
    parityCheckStatus { running errors date }
    disks { fsSize fsUsed fsFree }
    caches { fsSize fsUsed fsFree }
  }
  docker { containers { state } }
}";

static FIELDS: [Field; 1] = [Field {
    key: "secret",
    label_key: "field.unraid_secret",
    help_key: "field.unraid_secret_help",
    placeholder: "",
    kind: FieldKind::Text,
    required: true,
    secret: true,
}];

static METRICS: [MetricDef; 6] = [
    MetricDef {
        id: "containers_running",
        label_key: "metric.unraid_containers_running",
        format: Format::Number,
    },
    MetricDef {
        id: "containers_stopped",
        label_key: "metric.unraid_containers_stopped",
        format: Format::Number,
    },
    MetricDef {
        id: "array",
        label_key: "metric.unraid_array",
        format: Format::Text,
    },
    MetricDef {
        id: "disk_used",
        label_key: "metric.unraid_disk_used",
        format: Format::Percent,
    },
    MetricDef {
        id: "parity_errors",
        label_key: "metric.unraid_parity_errors",
        format: Format::Number,
    },
    MetricDef {
        id: "parity_last",
        label_key: "metric.unraid_parity_last",
        format: Format::Since,
    },
];

/// A server as one row of the top bar: its name and how loaded it is. Each
/// is missing when the answer does not have it in a usable form.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerRow {
    pub name: Option<String>,
    /// CPU load, 0..=100.
    pub cpu: Option<f64>,
    /// Memory in use, 0..=100.
    pub ram: Option<f64>,
    /// The fullest disk, 0..=100 (see the module).
    pub disk: Option<f64>,
}

/// The row of the server whose answer's `data` is `data`.
pub fn server_row(data: &Json) -> ServerRow {
    ServerRow {
        name: data
            .pointer("/info/os/hostname")
            .and_then(Json::as_str)
            .map(|name| api::clean_text(name, TEXT_CHARS))
            .filter(|name| !name.is_empty()),
        cpu: api::percent(data.pointer("/metrics/cpu/percentTotal")),
        ram: api::percent(data.pointer("/metrics/memory/percentTotal")),
        disk: disk_used(data),
    }
}

/// What is read of an answer. Each is missing when the answer does not have
/// it in a usable form.
#[derive(Debug, Default, PartialEq)]
struct Reading {
    running: Option<f64>,
    stopped: Option<f64>,
    array: Option<String>,
    disk_used: Option<f64>,
    parity_errors: Option<f64>,
    parity_last: Option<f64>,
}

/// A size or a count that is a JSON number or a string of one (`BigInt`
/// comes as either), finite and not below 0.
fn amount(value: Option<&Json>) -> Option<f64> {
    let n = match value? {
        Json::Number(n) => n.as_f64()?,
        Json::String(text) => f64::from_str(text.trim()).ok()?,
        _ => return None,
    };
    (n.is_finite() && n >= 0.0).then_some(n)
}

/// `used` as a percentage of `size`; none when there is no size. Only the
/// ratio counts, so the unit does not matter.
fn ratio(used: f64, size: f64) -> Option<f64> {
    (size > 0.0).then(|| (used / size * 100.0).clamp(0.0, 100.0))
}

/// What is used of one disk and how big it is, from its `fsSize` and `fsUsed`
/// (or `fsFree`), in the unit of the answer.
fn used_and_size(disk: &Json) -> Option<(f64, f64)> {
    let size = amount(disk.get("fsSize"))?;
    let used = amount(disk.get("fsUsed"))
        .or_else(|| amount(disk.get("fsFree")).map(|free| (size - free).max(0.0)))?;
    Some((used, size))
}

/// How full one disk is, from its `fsSize` and `fsUsed` (or `fsFree`).
fn one_disk(disk: &Json) -> Option<f64> {
    let (used, size) = used_and_size(disk)?;
    ratio(used, size)
}

/// The use of the fullest disk, or of the array when no disk says.
fn disk_used(data: &Json) -> Option<f64> {
    let array = data.get("array")?;
    ["disks", "caches"]
        .iter()
        .filter_map(|group| array.get(group).and_then(Json::as_array))
        .flatten()
        .filter_map(one_disk)
        .max_by(f64::total_cmp)
        .or_else(|| {
            ratio(
                amount(array.pointer("/capacity/kilobytes/used"))?,
                amount(array.pointer("/capacity/kilobytes/total"))?,
            )
        })
}

/// The time of the last parity check, as Unix seconds: an RFC 3339 time, or
/// a time without an offset, which is taken as UTC.
fn moment(text: &str) -> Option<f64> {
    let text = text.trim();
    let at = Timestamp::from_str(text).ok().or_else(|| {
        jiff::civil::DateTime::from_str(text)
            .ok()?
            .to_zoned(TimeZone::UTC)
            .ok()
            .map(|z| z.timestamp())
    })?;
    Some(at.as_second() as f64)
}

fn reading(data: &Json) -> Reading {
    let mut reading = Reading {
        disk_used: disk_used(data),
        ..Reading::default()
    };
    if let Some(containers) = data.pointer("/docker/containers").and_then(Json::as_array) {
        let (mut running, mut stopped) = (0.0, 0.0);
        for state in containers.iter().filter_map(|c| c.get("state")?.as_str()) {
            if state.eq_ignore_ascii_case("running") {
                running += 1.0;
            } else {
                stopped += 1.0;
            }
        }
        reading.running = Some(running);
        reading.stopped = Some(stopped);
    }
    if let Some(array) = data.get("array").filter(|a| a.is_object()) {
        reading.array = array
            .get("state")
            .and_then(Json::as_str)
            .map(|state| api::clean_text(state, TEXT_CHARS))
            .filter(|state| !state.is_empty());
        let parity = array.get("parityCheckStatus");
        reading.parity_errors = api::count(parity.and_then(|p| p.get("errors")));
        // A check that is running has no finished date to show.
        let running = parity
            .and_then(|p| p.get("running"))
            .and_then(Json::as_bool)
            .unwrap_or(false);
        if !running {
            reading.parity_last = parity
                .and_then(|p| p.get("date")?.as_str())
                .and_then(moment);
        }
    }
    reading
}

/// Whether the array is up. An answer without a state does not say it is not.
fn array_stopped(reading: &Reading) -> bool {
    reading
        .array
        .as_deref()
        .is_some_and(|state| !state.eq_ignore_ascii_case("started"))
}

/// Longest disk or container name kept in a block line, in characters.
const LINE_NAME_CHARS: usize = 48;

/// The fields only the details read: an error about one of them costs the
/// detail and nothing else.
const DETAIL_FIELDS: [&str; 4] = ["name", "temp", "names", "duration"];

/// Whether an error of a GraphQL answer is about something only the top bar or
/// the details read: `info`, `metrics`, `notifications`, or one of the
/// [`DETAIL_FIELDS`] of a disk, the check or a container.
fn is_extra(error: &Json) -> bool {
    let Some(path) = error.get("path").and_then(Json::as_array) else {
        return false;
    };
    match path.first().and_then(Json::as_str) {
        Some("info" | "metrics" | "notifications") => true,
        _ => {
            path.len() > 1
                && path
                    .last()
                    .and_then(Json::as_str)
                    .is_some_and(|leaf| DETAIL_FIELDS.contains(&leaf))
        }
    }
}

/// A number with one decimal, and none when it is whole («14», «12.3»).
fn short_number(n: f64) -> String {
    let rounded = (n * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}")
    }
}

/// «12.3 / 14 TB»: `used` of `size`, both in KiB, as decimal units of the size.
fn used_of(used_kib: f64, size_kib: f64) -> String {
    let (used, size) = (used_kib * 1024.0, size_kib * 1024.0);
    let (div, unit) = if size >= 1e12 {
        (1e12, "TB")
    } else if size >= 1e9 {
        (1e9, "GB")
    } else {
        (1e6, "MB")
    };
    format!(
        "{} / {} {unit}",
        short_number(used / div),
        short_number(size / div)
    )
}

/// One bar for a disk or a cache: its name, how full it is, the figures and
/// the temperature when the answer has it. A disk with no name or no file
/// system figures has no bar.
fn disk_bar(disk: &Json) -> Option<Bar> {
    let name = api::clean_text(disk.get("name")?.as_str()?, LINE_NAME_CHARS);
    if name.is_empty() {
        return None;
    }
    let (used, size) = used_and_size(disk)?;
    let mut text = used_of(used, size);
    if let Some(temp) = amount(disk.get("temp")).filter(|t| *t <= 150.0) {
        text.push_str(&format!(" · {} °C", short_number(temp)));
    }
    Some(Bar {
        label: name,
        pct: ratio(used, size)?,
        text,
    })
}

/// «Disker»: the data disks and then the caches. More than a block holds: the
/// fullest ones, in their order.
fn disk_bars(data: &Json) -> Option<Block> {
    let array = data.get("array")?;
    let mut bars: Vec<Bar> = ["disks", "caches"]
        .iter()
        .filter_map(|group| array.get(group).and_then(Json::as_array))
        .flatten()
        .filter_map(disk_bar)
        .collect();
    if bars.len() > MAX_ITEMS {
        let mut by_use: Vec<usize> = (0..bars.len()).collect();
        by_use.sort_by(|a, b| bars[*b].pct.total_cmp(&bars[*a].pct));
        let keep: Vec<usize> = by_use.into_iter().take(MAX_ITEMS).collect();
        let mut index = 0;
        bars.retain(|_| {
            index += 1;
            keep.contains(&(index - 1))
        });
    }
    (!bars.is_empty()).then(|| Block::Bars {
        title_key: "unraid_disks".into(),
        bars,
    })
}

/// The date of a time in Unix seconds (UTC), as «2026-11-01».
fn day(seconds: f64) -> Option<String> {
    let at = Timestamp::from_second(seconds as i64).ok()?;
    Some(at.to_zoned(TimeZone::UTC).date().to_string())
}

/// «8 h 12 min»: a length of time in seconds.
fn length(seconds: f64) -> String {
    let minutes = (seconds / 60.0).round() as u64;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "< 1 min".to_string(),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// «Paritetssjekk»: when it last ran, how long it took, and its errors. A check
/// that is running has no finished date or length to show.
fn parity_facts(data: &Json) -> Option<Block> {
    let parity = data
        .pointer("/array/parityCheckStatus")
        .filter(|p| p.is_object())?;
    let running = parity
        .get("running")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let mut rows = Vec::new();
    let mut row = |key: &str, value: Option<String>| {
        if let Some(value) = value {
            rows.push(Fact {
                label_key: Some(key.to_string()),
                label: None,
                value,
            });
        }
    };
    if !running {
        row(
            "unraid_parity_last",
            parity
                .get("date")
                .and_then(Json::as_str)
                .and_then(moment)
                .and_then(day),
        );
        row(
            "unraid_parity_duration",
            amount(parity.get("duration"))
                .filter(|s| *s > 0.0)
                .map(length),
        );
    }
    row(
        "unraid_parity_errors",
        api::count(parity.get("errors")).map(|n| format!("{n:.0}")),
    );
    (!rows.is_empty()).then(|| Block::Facts {
        title_key: "unraid_parity".into(),
        rows,
    })
}

/// A container's name: the first of `names` (Docker's have a leading `/`).
fn container_name(container: &Json) -> Option<String> {
    let names = container.get("names")?;
    let name = names.as_array().and_then(|l| l.first()).unwrap_or(names);
    let name = api::clean_text(name.as_str()?.trim_start_matches('/'), LINE_NAME_CHARS);
    (!name.is_empty()).then_some(name)
}

/// «Stoppede containere»: the ones that are not running, greyed.
fn stopped_list(data: &Json) -> Option<Block> {
    let containers = data.pointer("/docker/containers")?.as_array()?;
    let items: Vec<ListItem> = containers
        .iter()
        .filter(|c| {
            c.get("state")
                .and_then(Json::as_str)
                .is_some_and(|state| !state.eq_ignore_ascii_case("running"))
        })
        .filter_map(container_name)
        .map(|name| ListItem {
            status: Some(Status::Unknown),
            ..ListItem::new(name)
        })
        .collect();
    (!items.is_empty()).then(|| Block::List {
        title_key: "unraid_stopped".into(),
        items,
    })
}

/// «Varsler»: the unread notifications, when there are some (and the key may
/// read them).
fn notice_facts(data: &Json) -> Option<Block> {
    let unread = data.pointer("/notifications/overview/unread")?;
    let total = api::count(unread.get("total")).filter(|n| *n > 0.0)?;
    let mut rows = vec![("unraid_unread", total)];
    for (key, field) in [("unraid_alerts", "alert"), ("unraid_warnings", "warning")] {
        if let Some(n) = api::count(unread.get(field)).filter(|n| *n > 0.0) {
            rows.push((key, n));
        }
    }
    Some(Block::Facts {
        title_key: "unraid_notices".into(),
        rows: rows
            .into_iter()
            .map(|(key, n)| Fact {
                label_key: Some(key.to_string()),
                label: None,
                value: format!("{n:.0}"),
            })
            .collect(),
    })
}

/// The detail blocks of the `data` of an answer: the disks' bars, the parity
/// check's facts, the stopped containers and the unread notifications. Names
/// go only into these blocks (the poller cuts them down; see
/// [`super::block`]).
pub fn details(data: &Json) -> Vec<Block> {
    [
        disk_bars(data),
        parity_facts(data),
        stopped_list(data),
        notice_facts(data),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn snapshot(reading: Reading) -> Snapshot {
    let stopped = array_stopped(&reading);
    let errors = reading.parity_errors.unwrap_or(0.0);
    let disk = reading.disk_used.unwrap_or(0.0);
    let status = if stopped || errors > 0.0 || disk >= DISK_BAD {
        Status::Bad
    } else if disk >= DISK_WARN {
        Status::Warn
    } else {
        Status::Ok
    };
    let mut snapshot = Snapshot::new(status);
    let mut values = BTreeMap::new();
    for (id, number) in [
        ("containers_running", reading.running),
        ("containers_stopped", reading.stopped),
        ("disk_used", reading.disk_used),
        ("parity_errors", reading.parity_errors),
        ("parity_last", reading.parity_last),
    ] {
        if let Some(number) = number {
            values.insert(id.to_string(), Value::num(number));
        }
    }
    if let Some(state) = reading.array {
        values.insert("array".to_string(), Value::text(state));
    }
    snapshot.values = values;
    if stopped {
        snapshot
            .needs
            .push(need(ARRAY_NEED, Level::Bad, "need.unraid_array", None, 1.0));
    }
    if errors > 0.0 {
        snapshot.needs.push(need(
            PARITY_NEED,
            Level::Bad,
            "need.unraid_parity_errors",
            Some(errors),
            errors,
        ));
    }
    if disk >= DISK_WARN {
        let level = if disk >= DISK_BAD {
            Level::Bad
        } else {
            Level::Warn
        };
        snapshot.needs.push(need(
            DISK_NEED,
            level,
            "need.unraid_disk",
            Some(disk.floor()),
            disk,
        ));
    }
    snapshot
}

/// A need whose text takes only a count (the array's has none).
fn need(key: &str, level: Level, label_key: &str, count: Option<f64>, score: f64) -> Need {
    Need {
        key: key.to_string(),
        level,
        label_key: label_key.to_string(),
        params: count
            .map(|n| BTreeMap::from([("count".to_string(), format!("{n:.0}"))]))
            .unwrap_or_default(),
        score,
        since: None,
    }
}

/// Whether every error of a GraphQL answer says that the key may not read it
/// (and there is at least one).
fn all_refused(errors: Option<&Json>) -> bool {
    let Some(list) = errors.and_then(Json::as_array).filter(|l| !l.is_empty()) else {
        return false;
    };
    list.iter().all(|error| {
        error
            .pointer("/extensions/code")
            .and_then(Json::as_str)
            .is_some_and(|code| {
                matches!(
                    code.to_ascii_uppercase().as_str(),
                    "UNAUTHENTICATED" | "UNAUTHORIZED" | "FORBIDDEN"
                )
            })
    })
}

/// What the errors of an answer that has data say about the part of it that
/// matters: `None` when they are only about the extras (`info`, `metrics`, the
/// details' fields; see [`is_extra`]) or there are none, else why (the key may not read it, or something else).
fn core_error(errors: Option<&Json>) -> Option<FetchError> {
    let list = errors.and_then(Json::as_array)?;
    let core: Vec<&Json> = list.iter().filter(|error| !is_extra(error)).collect();
    if core.is_empty() {
        None
    } else if all_refused(Some(&Json::Array(core.into_iter().cloned().collect()))) {
        Some(FetchError::Auth)
    } else {
        Some(FetchError::Invalid)
    }
}

/// Whether the server rejected the query itself (it asks for a field this
/// Unraid does not have): `errors` with a validation code and nothing usable
/// in `data`.
fn query_rejected(doc: &Json) -> bool {
    doc.get("data").is_none_or(|data| !data.is_object())
        && doc
            .get("errors")
            .and_then(Json::as_array)
            .is_some_and(|list| {
                list.iter().any(|error| {
                    error.pointer("/extensions/code").and_then(Json::as_str)
                        == Some("GRAPHQL_VALIDATION_FAILED")
                })
            })
}

/// The snapshot of a GraphQL answer.
fn answer(doc: &Json) -> Result<Snapshot, FetchError> {
    let data = doc.get("data").filter(|data| data.is_object());
    let reading = data.map(reading).filter(|r| *r != Reading::default());
    match reading {
        Some(reading) => {
            let mut snapshot =
                snapshot(reading).with_details(data.map(details).unwrap_or_default());
            // A part that matters could not be read: not quite well.
            if let Some(error) = core_error(doc.get("errors")) {
                if snapshot.status == Status::Ok {
                    snapshot.status = Status::Warn;
                }
                snapshot.error = Some(error.to_string());
            }
            Ok(snapshot)
        }
        None if all_refused(doc.get("errors")) => Err(FetchError::Auth),
        None => Err(FetchError::Invalid),
    }
}

/// One POST of `query`.
async fn ask(
    ctx: &FetchCtx,
    url: &url::Url,
    key: &Credential,
    query: &str,
) -> Result<Json, FetchError> {
    api::post_json(&ctx.http, url.clone(), key, &json!({ "query": query }))
        .await?
        .found()
}

async fn read(ctx: &FetchCtx) -> Result<Snapshot, FetchError> {
    let base = api::base_url(&ctx.settings)?;
    let key = Credential::header(KEY_HEADER, ctx.secret.as_deref())?;
    let url = api::endpoint(&base, &["graphql"], None)?;
    let address = url.to_string();
    if !CORE_ONLY.is_old(&address, Instant::now()) {
        match ask(ctx, &url, &key, QUERY).await {
            Err(FetchError::Status(400)) => {}
            Ok(doc) if query_rejected(&doc) => {}
            Ok(doc) => {
                CORE_ONLY.forget(&address);
                return answer(&doc);
            }
            Err(err) => return Err(err),
        }
        CORE_ONLY.mark(&address, Instant::now());
    }
    answer(&ask(ctx, &url, &key, CORE_QUERY).await?)
}

impl Integration for UnraidKind {
    fn kind(&self) -> &'static str {
        "unraid"
    }

    fn label_key(&self) -> &'static str {
        "kind.unraid"
    }

    fn fields(&self) -> &'static [Field] {
        &FIELDS
    }

    fn metrics(&self) -> &'static [MetricDef] {
        &METRICS
    }

    fn default_interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn min_interval(&self) -> Duration {
        Duration::from_secs(30)
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
    fn the_server_row_reads_name_cpu_memory_and_the_fullest_disk() {
        let data = json!({
            "info": { "os": { "hostname": "Tower\u{202E}" } },
            "metrics": { "cpu": { "percentTotal": 12.5 }, "memory": { "percentTotal": 140 } },
            "array": { "disks": [
                { "fsSize": "1000", "fsUsed": "250" },
                { "fsSize": "1000", "fsUsed": "900" },
            ] },
        });
        let row = server_row(&data);
        assert_eq!(row.name.as_deref(), Some("Tower"));
        assert_eq!(row.cpu, Some(12.5));
        assert_eq!(row.ram, Some(100.0), "clamped");
        assert_eq!(row.disk, Some(90.0));
    }

    #[test]
    fn a_server_row_with_nothing_has_nothing() {
        assert_eq!(server_row(&json!({})), ServerRow::default());
        let odd = json!({
            "info": { "os": { "hostname": 5 } },
            "metrics": { "cpu": null, "memory": { "percentTotal": "x" } },
            "array": null,
        });
        assert_eq!(server_row(&odd), ServerRow::default());
    }

    #[test]
    fn the_unit_of_the_file_system_figures_does_not_matter() {
        // KiB or bytes, a number or a string: the same ratio.
        for (size, used) in [
            (json!(1_000_000), json!(870_000)),
            (json!("1000000000"), json!("870000000")),
            (json!("1000000"), json!(870_000)),
        ] {
            let disk = json!({ "fsSize": size, "fsUsed": used });
            let got = one_disk(&disk).unwrap();
            assert!((got - 87.0).abs() < 1e-9, "{got}");
        }
        // Free instead of used.
        let free = json!({ "fsSize": 1000, "fsFree": 130 });
        assert!((one_disk(&free).unwrap() - 87.0).abs() < 1e-9);
        // Nothing to divide, or odd figures.
        for odd in [
            json!({}),
            json!({ "fsSize": 0, "fsUsed": 0 }),
            json!({ "fsSize": null, "fsUsed": 5 }),
            json!({ "fsSize": -4, "fsUsed": 1 }),
            json!({ "fsSize": "big", "fsUsed": 1 }),
            json!({ "fsSize": 100 }),
        ] {
            assert_eq!(one_disk(&odd), None, "{odd}");
        }
        // More used than there is: capped.
        assert_eq!(
            one_disk(&json!({ "fsSize": 10, "fsUsed": 50 })),
            Some(100.0)
        );
    }

    #[test]
    fn the_array_capacity_is_used_when_no_disk_has_figures() {
        let data = json!({ "array": {
            "capacity": { "kilobytes": { "used": "250", "total": "1000" } },
            "disks": [{ "fsSize": null, "fsUsed": null }],
        } });
        assert_eq!(disk_used(&data), Some(25.0));
        assert_eq!(disk_used(&json!({ "array": {} })), None);
    }

    #[test]
    fn the_last_parity_check_is_a_time_with_or_without_an_offset() {
        assert_eq!(moment("2026-11-01T02:00:00Z"), Some(1_793_498_400.0));
        assert_eq!(moment("2026-11-01T03:00:00+01:00"), Some(1_793_498_400.0));
        assert_eq!(moment("2026-11-01T02:00:00"), Some(1_793_498_400.0));
        assert_eq!(moment("yesterday"), None);
        assert_eq!(moment(""), None);
    }

    #[test]
    fn a_running_check_has_no_last_date() {
        let data = json!({ "array": { "state": "STARTED", "parityCheckStatus": {
            "running": true, "errors": 0, "date": "2026-11-01T02:00:00Z",
        } } });
        let r = reading(&data);
        assert_eq!(r.parity_last, None);
        assert_eq!(r.parity_errors, Some(0.0));
    }

    #[test]
    fn nothing_usable_is_no_reading() {
        for doc in [
            json!({}),
            json!({ "data": null }),
            json!({ "data": {} }),
            json!({ "data": { "array": null, "docker": null } }),
            json!([]),
        ] {
            assert_eq!(answer(&doc).unwrap_err(), FetchError::Invalid, "{doc}");
        }
    }
}
