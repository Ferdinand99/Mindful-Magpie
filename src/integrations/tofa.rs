//! The `tofa` kind («Tofa»): a media server (a third-party Plex alternative)
//! read through its public beta API with a server API key.
//!
//! The API may differ from one server version to the next, so the kind
//! feature-detects. A fetch starts with `GET /api/v1/health` (no login needed;
//! it is the «server answers» probe and runs every time), then
//! `GET /api/v1/system/info` (version, `capabilities`; remembered for an hour
//! per address), then only the calls the capabilities allow:
//!
//! - every fetch: `GET /api/v1/system/playback/sessions` (streams, transcodes);
//! - every [`SLOW_EVERY`]th fetch: `GET /api/v1/libraries/health`,
//!   `GET /api/v1/system/tasks` and `GET /api/v1/media/changes`. In between,
//!   the last values of those calls are carried forward, so the numbers do not
//!   flicker.
//!
//! An empty or missing capability list (an older server) does not gate
//! anything. An endpoint that answers 404 is left alone for an hour, as the
//! other kinds do ([`OldAddresses`]).
//!
//! What the kind remembers between fetches lives in memory, per address, in a
//! bounded list ([`MEMORY_MAX`]): the info, a fetch counter, the last slow
//! values and the revision the «new in library» count starts from.
//!
//! **New in library** is the number of media items that changed with an
//! `upsert` since the start of the UTC day, counted from `/media/changes`
//! (which has no timestamps). An item counts once however often it changed,
//! and not at all when its last change is a delete. A metadata refresh is an
//! upsert too, so the number can run a little high. At most
//! [`CHANGES_LIMIT`] changes are read; past that the number is a lower bound.
//!
//! The key is sent as `Authorization: Bearer`, marked sensitive, and acts as
//! an admin in Tofa: it is a password. Titles, library names and user names
//! from Tofa are never kept: the reading holds numbers and the server's
//! version only.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use reqwest::header::HeaderValue;
use serde_json::Value as Json;
use url::Url;

use super::api::{self, OldAddresses, Reply};
use super::{
    BoxFuture, FetchCtx, FetchError, Field, FieldKind, Format, Integration, Level, MetricDef, Need,
    Snapshot, Status, Value,
};

/// The only instance.
pub static TOFA: TofaKind = TofaKind;

/// See the module.
pub struct TofaKind;

/// The capability names the kind looks for before it calls an endpoint. The
/// public spec does not list the names (only that `capabilities` is a list of
/// strings), so these are **unconfirmed**: check them against a running
/// server's `GET /api/v1/system/info`. A server that advertises capabilities
/// but not the one of an endpoint is not asked for that endpoint.
pub const CAP_SESSIONS: &str = "playback.sessions";
pub const CAP_LIBRARIES: &str = "libraries.health";
pub const CAP_TASKS: &str = "system.tasks";
pub const CAP_CHANGES: &str = "media.changes";

/// The key of the need «the server does not answer».
pub const DOWN_NEED: &str = "tofa:down";
/// The key of the need «libraries with errors».
pub const LIBRARY_NEED: &str = "tofa:library";

/// Every how-many-th fetch refreshes the slow calls (the first one does).
pub const SLOW_EVERY: u64 = 4;
/// How long the info of an address is remembered.
pub const INFO_FOR: Duration = Duration::from_secs(60 * 60);
/// Most addresses the kind remembers; past that the one used longest ago goes.
pub const MEMORY_MAX: usize = 64;
/// How many changes one `/media/changes` call asks for.
pub const CHANGES_LIMIT: u32 = 500;

const VERSION_CHARS: usize = 40;
const CAPABILITIES_MAX: usize = 256;
const CAPABILITY_CHARS: usize = 64;
const SECONDS_PER_DAY: i64 = 86_400;

/// Task statuses that mean the task is no longer running.
const FINISHED: [&str; 8] = [
    "completed",
    "complete",
    "done",
    "finished",
    "succeeded",
    "success",
    "cancelled",
    "canceled",
];

/// The addresses that answered 404 (key: the endpoint's address).
static OLD: OldAddresses = OldAddresses::new();

/// What is remembered per address.
static MEMORY: Mutex<VecDeque<(String, Memory)>> = Mutex::new(VecDeque::new());

static FIELDS: [Field; 1] = [Field {
    key: "secret",
    label_key: "field.tofa_secret",
    help_key: "field.tofa_secret_help",
    placeholder: "",
    kind: FieldKind::Text,
    required: true,
    secret: true,
}];

static METRICS: [MetricDef; 6] = [
    MetricDef {
        id: "streams",
        label_key: "metric.tofa_streams",
        format: Format::Number,
    },
    MetricDef {
        id: "transcodes",
        label_key: "metric.tofa_transcodes",
        format: Format::Number,
    },
    MetricDef {
        id: "new_items",
        label_key: "metric.tofa_new_items",
        format: Format::Number,
    },
    MetricDef {
        id: "scans_running",
        label_key: "metric.tofa_scans_running",
        format: Format::Number,
    },
    MetricDef {
        id: "libraries_unhealthy",
        label_key: "metric.tofa_libraries_unhealthy",
        format: Format::Number,
    },
    MetricDef {
        id: "version",
        label_key: "metric.tofa_version",
        format: Format::Text,
    },
];

/// One call: its path and the capability it needs (none: always allowed).
#[derive(Debug, Clone, Copy)]
struct Endpoint {
    path: &'static [&'static str],
    capability: Option<&'static str>,
}

const HEALTH: Endpoint = Endpoint {
    path: &["api", "v1", "health"],
    capability: None,
};
const INFO: Endpoint = Endpoint {
    path: &["api", "v1", "system", "info"],
    capability: None,
};
const SESSIONS: Endpoint = Endpoint {
    path: &["api", "v1", "system", "playback", "sessions"],
    capability: Some(CAP_SESSIONS),
};
const LIBRARIES: Endpoint = Endpoint {
    path: &["api", "v1", "libraries", "health"],
    capability: Some(CAP_LIBRARIES),
};
const TASKS: Endpoint = Endpoint {
    path: &["api", "v1", "system", "tasks"],
    capability: Some(CAP_TASKS),
};
const CHANGES: Endpoint = Endpoint {
    path: &["api", "v1", "media", "changes"],
    capability: Some(CAP_CHANGES),
};

impl Integration for TofaKind {
    fn kind(&self) -> &'static str {
        "tofa"
    }

    fn label_key(&self) -> &'static str {
        "kind.tofa"
    }

    fn fields(&self) -> &'static [Field] {
        &FIELDS
    }

    fn metrics(&self) -> &'static [MetricDef] {
        &METRICS
    }

    fn default_interval(&self) -> Duration {
        Duration::from_secs(15)
    }

    fn min_interval(&self) -> Duration {
        Duration::from_secs(15)
    }

    fn fetch<'a>(&'a self, ctx: &'a FetchCtx) -> BoxFuture<'a, Result<Snapshot, FetchError>> {
        Box::pin(async move {
            let base = api::base_url(&ctx.settings)?;
            let auth = api::bearer(ctx.secret.as_deref())?;
            let key = base.to_string();
            let now = Instant::now();
            let before = remember(&key, |m| {
                let before = m.clone();
                m.fetches = m.fetches.wrapping_add(1);
                before
            });
            let slow_due = before.fetches % SLOW_EVERY == 0;

            // Is the server there? Any failure but a refused key says no.
            let health = match get(ctx, &base, &auth, &HEALTH).await {
                Ok(Reply::Found(doc)) => doc,
                Ok(Reply::NotFound) => return Ok(down(FetchError::Status(404))),
                Err(FetchError::Auth) => return Err(FetchError::Auth),
                Err(err) => return Ok(down(err)),
            };

            // What can it do? Remembered for an hour. A failure other than a
            // refused key leaves the capabilities unknown (nothing is gated)
            // until the next fetch.
            let mut info = before
                .info
                .filter(|i| now.saturating_duration_since(i.at) < INFO_FOR);
            if info.is_none() {
                match get(ctx, &base, &auth, &INFO).await {
                    Ok(Reply::Found(doc)) => info = Some(read_info(&doc, now)),
                    Ok(Reply::NotFound) => info = Some(Info::unknown(now)),
                    Err(FetchError::Auth) => return Err(FetchError::Auth),
                    Err(_) => {}
                }
                if let Some(fresh) = &info {
                    remember(&key, |m| m.info = Some(fresh.clone()));
                }
            }
            let caps: &[String] = info.as_ref().map_or(&[], |i| i.capabilities.as_slice());
            let version = info
                .as_ref()
                .and_then(|i| i.version.clone())
                .or_else(|| text_of(&health, "version"));

            let mut error = None;

            let sessions = call(ctx, &base, &auth, &SESSIONS, None, caps)
                .await
                .and_then(|doc| doc.as_ref().map(read_sessions).transpose());
            let sessions = match got(sessions, &mut error)? {
                Got::Value(counts) => Some(counts),
                Got::Absent | Got::Failed => None,
            };

            let mut slow = before.slow;
            if slow_due {
                let today = Timestamp::now().as_second().div_euclid(SECONDS_PER_DAY);
                let (query, rebase) = match slow.baseline {
                    Some(b) if b.day == today => (
                        format!("since_revision={}&limit={CHANGES_LIMIT}", b.revision),
                        false,
                    ),
                    _ => ("limit=1".to_string(), true),
                };
                let (libraries, tasks, changes) = tokio::join!(
                    call(ctx, &base, &auth, &LIBRARIES, None, caps),
                    call(ctx, &base, &auth, &TASKS, None, caps),
                    call(ctx, &base, &auth, &CHANGES, Some(&query), caps),
                );
                let libraries =
                    libraries.and_then(|doc| doc.as_ref().map(read_libraries).transpose());
                let tasks = tasks.and_then(|doc| doc.as_ref().map(read_tasks).transpose());
                let changes = changes.and_then(|doc| doc.as_ref().map(read_changes).transpose());

                match got(libraries, &mut error)? {
                    Got::Value(n) => slow.libraries_unhealthy = Some(n),
                    Got::Absent => slow.libraries_unhealthy = None,
                    Got::Failed => {}
                }
                match got(tasks, &mut error)? {
                    Got::Value((running, failed)) => {
                        slow.scans_running = Some(running);
                        slow.scans_failed = Some(failed);
                    }
                    Got::Absent => {
                        slow.scans_running = None;
                        slow.scans_failed = None;
                    }
                    Got::Failed => {}
                }
                match got(changes, &mut error)? {
                    Got::Value(answer) => apply_changes(&mut slow, &answer, rebase, today),
                    Got::Absent => slow.new_items = None,
                    Got::Failed => {}
                }
                remember(&key, |m| m.slow = slow.clone());
            }

            Ok(reading(version, sessions, &slow, error))
        })
    }
}

// ---------------------------------------------------------------------------
// Memory

/// What is remembered of an address between fetches.
#[derive(Debug, Clone, Default)]
struct Memory {
    info: Option<Info>,
    /// How many fetches began (it decides which one refreshes the slow calls).
    fetches: u64,
    slow: Slow,
}

/// The server's `system/info`, as far as the kind uses it.
#[derive(Debug, Clone)]
struct Info {
    version: Option<String>,
    capabilities: Vec<String>,
    at: Instant,
}

impl Info {
    /// No capabilities known, so none gates anything.
    fn unknown(at: Instant) -> Self {
        Info {
            version: None,
            capabilities: Vec::new(),
            at,
        }
    }
}

/// The values of the slow calls, carried between their refreshes.
#[derive(Debug, Clone, Default, PartialEq)]
struct Slow {
    libraries_unhealthy: Option<f64>,
    scans_running: Option<f64>,
    scans_failed: Option<f64>,
    new_items: Option<f64>,
    baseline: Option<Baseline>,
}

/// The revision «new in library» counts from, and the UTC day (days since the
/// epoch) it was taken, so a new day starts again.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Baseline {
    revision: i64,
    day: i64,
}

/// Runs `f` on the memory of `key` (made if it is not there), which then
/// counts as the most recently used. The list holds at most [`MEMORY_MAX`]
/// addresses.
fn remember<R>(key: &str, f: impl FnOnce(&mut Memory) -> R) -> R {
    // A panic elsewhere cannot leave the list in a state that matters.
    let mut list = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    remember_in(&mut list, key, f)
}

fn remember_in<R>(
    list: &mut VecDeque<(String, Memory)>,
    key: &str,
    f: impl FnOnce(&mut Memory) -> R,
) -> R {
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

// ---------------------------------------------------------------------------
// Calls

/// One GET of a fixed endpoint, with the key.
async fn get(
    ctx: &FetchCtx,
    base: &Url,
    auth: &HeaderValue,
    endpoint: &Endpoint,
) -> Result<Reply<Json>, FetchError> {
    let url = api::endpoint(base, endpoint.path, None)?;
    api::get_json(&ctx.http, url, auth).await
}

/// Whether the capabilities allow the endpoint: an empty list does (the
/// server does not say what it can do).
fn allowed(capabilities: &[String], endpoint: &Endpoint) -> bool {
    match endpoint.capability {
        Some(wanted) if !capabilities.is_empty() => capabilities.iter().any(|c| c == wanted),
        _ => true,
    }
}

/// One GET of a data endpoint. `None`: not asked (the capabilities do not
/// allow it, or the address answered 404 less than an hour ago) or it answered
/// 404 now.
async fn call(
    ctx: &FetchCtx,
    base: &Url,
    auth: &HeaderValue,
    endpoint: &Endpoint,
    query: Option<&str>,
    capabilities: &[String],
) -> Result<Option<Json>, FetchError> {
    if !allowed(capabilities, endpoint) {
        return Ok(None);
    }
    let old_key = api::endpoint(base, endpoint.path, None)?.to_string();
    if OLD.is_old(&old_key, Instant::now()) {
        return Ok(None);
    }
    let url = api::endpoint(base, endpoint.path, query)?;
    match api::get_json::<Json>(&ctx.http, url, auth).await? {
        Reply::Found(doc) => {
            OLD.forget(&old_key);
            Ok(Some(doc))
        }
        Reply::NotFound => {
            OLD.mark(&old_key, Instant::now());
            Ok(None)
        }
    }
}

/// What came of one data call.
enum Got<T> {
    Value(T),
    /// Not asked, or the server has no such endpoint.
    Absent,
    /// It failed; the reason is in the error.
    Failed,
}

/// Sorts a call's result: a refused key ends the fetch, any other failure
/// is noted (the first one) and costs only that call's numbers.
fn got<T>(
    result: Result<Option<T>, FetchError>,
    error: &mut Option<FetchError>,
) -> Result<Got<T>, FetchError> {
    match result {
        Ok(Some(value)) => Ok(Got::Value(value)),
        Ok(None) => Ok(Got::Absent),
        Err(FetchError::Auth) => Err(FetchError::Auth),
        Err(err) => {
            error.get_or_insert(err);
            Ok(Got::Failed)
        }
    }
}

// ---------------------------------------------------------------------------
// Reading the answers

fn text_of(doc: &Json, field: &str) -> Option<String> {
    doc.get(field)
        .and_then(Json::as_str)
        .map(|text| api::clean_text(text, VERSION_CHARS))
        .filter(|text| !text.is_empty())
}

fn read_info(doc: &Json, at: Instant) -> Info {
    let capabilities = doc
        .get("capabilities")
        .and_then(Json::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Json::as_str)
                .map(|c| api::clean_text(c, CAPABILITY_CHARS))
                .filter(|c| !c.is_empty())
                .take(CAPABILITIES_MAX)
                .collect()
        })
        .unwrap_or_default();
    Info {
        version: text_of(doc, "version"),
        capabilities,
        at,
    }
}

fn array(doc: &Json) -> Result<&Vec<Json>, FetchError> {
    doc.as_array().ok_or(FetchError::Invalid)
}

/// (streams, transcodes) of a sessions answer. Every listed session is a
/// stream, paused or not; the ones with `play_method` `Transcode` are
/// transcodes.
fn read_sessions(doc: &Json) -> Result<(f64, f64), FetchError> {
    let list = array(doc)?;
    let transcodes = list
        .iter()
        .filter(|s| s.get("play_method").and_then(Json::as_str) == Some("Transcode"))
        .count();
    Ok((list.len() as f64, transcodes as f64))
}

fn positive(value: Option<&Json>) -> bool {
    api::count(value).is_some_and(|n| n > 0.0)
}

/// Whether a library has something that needs a person: titles that are
/// unavailable, files that fail to probe, or folders that overlap another
/// library's. Titles that are only unmatched do not count (most libraries
/// have some).
fn library_unhealthy(library: &Json) -> bool {
    positive(library.get("unavailable_titles"))
        || positive(library.get("probe_failed_files"))
        || library
            .get("folder_overlaps")
            .and_then(Json::as_array)
            .is_some_and(|overlaps| !overlaps.is_empty())
}

/// How many libraries are unhealthy.
fn read_libraries(doc: &Json) -> Result<f64, FetchError> {
    Ok(array(doc)?.iter().filter(|l| library_unhealthy(l)).count() as f64)
}

/// (tasks running, tasks failed) of a tasks answer. The endpoint lists the
/// tasks that are running now; the spec does not list the status values, so a
/// task is failed when its status has `fail` or `error` in it, finished
/// (not counted) when it is one of [`FINISHED`], and running otherwise.
fn read_tasks(doc: &Json) -> Result<(f64, f64), FetchError> {
    let (mut running, mut failed) = (0.0, 0.0);
    for task in array(doc)? {
        let status = task
            .get("status")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if status.contains("fail") || status.contains("error") {
            failed += 1.0;
        } else if !FINISHED.contains(&status.as_str()) {
            running += 1.0;
        }
    }
    Ok((running, failed))
}

/// What a `/media/changes` answer tells.
#[derive(Debug, PartialEq)]
struct Changes {
    latest: i64,
    full_sync: bool,
    fresh: f64,
}

fn read_changes(doc: &Json) -> Result<Changes, FetchError> {
    let latest = doc
        .get("latest_revision")
        .and_then(Json::as_i64)
        .filter(|n| *n >= 0)
        .ok_or(FetchError::Invalid)?;
    let list: &[Json] = doc
        .get("changes")
        .and_then(Json::as_array)
        .map_or(&[], Vec::as_slice);
    Ok(Changes {
        latest,
        full_sync: doc.get("requires_full_sync").and_then(Json::as_bool) == Some(true),
        fresh: fresh_items(list),
    })
}

/// The media items whose latest change in the list is an upsert.
fn fresh_items(changes: &[Json]) -> f64 {
    let mut last: HashMap<&str, (i64, bool)> = HashMap::new();
    for change in changes {
        let Some(id) = change.get("media_id").and_then(Json::as_str) else {
            continue;
        };
        let revision = change.get("revision").and_then(Json::as_i64).unwrap_or(0);
        let upsert = change.get("change_kind").and_then(Json::as_str) == Some("upsert");
        let entry = last.entry(id).or_insert((revision, upsert));
        if revision >= entry.0 {
            *entry = (revision, upsert);
        }
    }
    last.values().filter(|(_, upsert)| *upsert).count() as f64
}

/// Takes a `/media/changes` answer into the slow values. After the call that
/// sets the baseline (`rebase`) or an answer that says the changes are too
/// many to list (`requires_full_sync`), the count starts again from the
/// answer's latest revision.
fn apply_changes(slow: &mut Slow, answer: &Changes, rebase: bool, today: i64) {
    if rebase || answer.full_sync {
        slow.baseline = Some(Baseline {
            revision: answer.latest,
            day: today,
        });
        slow.new_items = Some(0.0);
    } else {
        slow.new_items = Some(answer.fresh);
    }
}

// ---------------------------------------------------------------------------
// The reading

/// A need whose only parameter is how many.
fn counted_need(key: &str, level: Level, label_key: &str, count: f64) -> Need {
    Need {
        key: key.to_string(),
        level,
        label_key: label_key.to_string(),
        params: [("count".to_string(), format!("{count}"))].into(),
        score: count,
        since: None,
    }
}

/// The reading of a server that does not answer.
fn down(err: FetchError) -> Snapshot {
    let mut snapshot = Snapshot::failed(&err);
    snapshot.needs.push(Need {
        key: DOWN_NEED.to_string(),
        level: Level::Bad,
        label_key: "need.tofa_down".to_string(),
        params: Default::default(),
        score: 1.0,
        since: None,
    });
    snapshot
}

/// The reading of a server that answers: bad when a library is unhealthy,
/// warn when a task failed or a call failed, ok otherwise.
fn reading(
    version: Option<String>,
    sessions: Option<(f64, f64)>,
    slow: &Slow,
    error: Option<FetchError>,
) -> Snapshot {
    let mut snapshot = Snapshot::new(Status::Ok);
    let mut put = |id: &str, value: Option<f64>| {
        if let Some(n) = value {
            snapshot.values.insert(id.to_string(), Value::num(n));
        }
    };
    put("streams", sessions.map(|(streams, _)| streams));
    put("transcodes", sessions.map(|(_, transcodes)| transcodes));
    put("new_items", slow.new_items);
    put("scans_running", slow.scans_running);
    put("libraries_unhealthy", slow.libraries_unhealthy);
    if let Some(version) = version {
        snapshot
            .values
            .insert("version".to_string(), Value::text(version));
    }

    let unhealthy = slow.libraries_unhealthy.unwrap_or(0.0);
    if unhealthy > 0.0 {
        snapshot.needs.push(counted_need(
            LIBRARY_NEED,
            Level::Bad,
            "need.tofa_library",
            unhealthy,
        ));
        snapshot.status = Status::Bad;
    } else if slow.scans_failed.unwrap_or(0.0) > 0.0 || error.is_some() {
        snapshot.status = Status::Warn;
    }
    snapshot.error = error.map(|e| e.to_string());
    snapshot
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn caps(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn the_capabilities_gate_only_when_there_are_some() {
        assert!(allowed(&[], &TASKS));
        assert!(allowed(&caps(&[CAP_TASKS]), &TASKS));
        assert!(!allowed(&caps(&[CAP_SESSIONS]), &TASKS));
        assert!(allowed(&caps(&["other"]), &HEALTH));
        assert!(allowed(&caps(&["other"]), &INFO));
    }

    #[test]
    fn the_memory_is_bounded_and_the_least_recently_used_goes() {
        let mut list = VecDeque::new();
        for i in 0..MEMORY_MAX + 10 {
            remember_in(&mut list, &format!("http://h{i}/"), |m| m.fetches = 1);
        }
        assert_eq!(list.len(), MEMORY_MAX);
        assert!(list.iter().all(|(k, _)| k != "http://h0/"));
        // Using an old one keeps it.
        remember_in(&mut list, "http://h10/", |m| m.fetches += 1);
        for i in 0..MEMORY_MAX - 1 {
            remember_in(&mut list, &format!("http://n{i}/"), |_| ());
        }
        assert_eq!(list.len(), MEMORY_MAX);
        assert!(
            list.iter()
                .any(|(k, m)| k == "http://h10/" && m.fetches == 2)
        );
        assert!(list.iter().all(|(k, _)| k != "http://h11/"));
    }

    #[test]
    fn the_info_is_cut_and_cleaned() {
        let now = Instant::now();
        let long = "9".repeat(200);
        let info = read_info(
            &json!({
                "version": format!("0.13.1\u{202E}{long}"),
                "capabilities": ["a", "", 5, "b\u{0}c", null],
            }),
            now,
        );
        assert_eq!(info.version.as_deref().map(|v| v.chars().count()), Some(40));
        assert!(!info.version.unwrap().contains('\u{202E}'));
        assert_eq!(info.capabilities, ["a", "bc"]);
        let many: Vec<String> = (0..1000).map(|i| format!("c{i}")).collect();
        assert_eq!(
            read_info(&json!({ "capabilities": many }), now)
                .capabilities
                .len(),
            CAPABILITIES_MAX
        );
    }

    #[test]
    fn a_media_item_counts_once_and_not_when_it_was_deleted() {
        let change = |id: &str, revision: i64, kind: &str| json!({ "media_id": id, "revision": revision, "change_kind": kind, "library_id": null });
        let list = [
            change("a", 1, "upsert"),
            change("a", 2, "upsert"),
            change("b", 3, "upsert"),
            change("b", 5, "delete"),
            change("c", 6, "delete"),
            change("c", 4, "upsert"),
            json!({ "revision": 7, "change_kind": "upsert" }),
            json!("odd"),
        ];
        assert_eq!(fresh_items(&list), 1.0, "only a is a new item");
        assert_eq!(fresh_items(&[]), 0.0);
    }

    #[test]
    fn the_baseline_restarts_the_count() {
        let mut slow = Slow::default();
        let answer = |latest, full_sync, fresh| Changes {
            latest,
            full_sync,
            fresh,
        };
        apply_changes(&mut slow, &answer(100, false, 1.0), true, 5);
        assert_eq!(slow.new_items, Some(0.0));
        assert_eq!(
            slow.baseline,
            Some(Baseline {
                revision: 100,
                day: 5
            })
        );
        apply_changes(&mut slow, &answer(130, false, 4.0), false, 5);
        assert_eq!(slow.new_items, Some(4.0));
        assert_eq!(
            slow.baseline,
            Some(Baseline {
                revision: 100,
                day: 5
            })
        );
        apply_changes(&mut slow, &answer(900, true, 500.0), false, 5);
        assert_eq!(slow.new_items, Some(0.0));
        assert_eq!(
            slow.baseline,
            Some(Baseline {
                revision: 900,
                day: 5
            })
        );
    }

    #[test]
    fn tasks_are_running_failed_or_finished() {
        let doc = json!([
            { "status": "running" },
            { "status": "Queued" },
            {},
            { "status": "failed" },
            { "status": "ProbeError" },
            { "status": "completed" },
        ]);
        assert_eq!(read_tasks(&doc).unwrap(), (3.0, 2.0));
        assert_eq!(read_tasks(&json!({})), Err(FetchError::Invalid));
    }
}
