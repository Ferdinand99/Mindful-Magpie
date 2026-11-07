//! The `tofa` kind («Tofa»): a media server (a third-party Plex alternative)
//! read through its public beta API with a server API key.
//!
//! A fetch starts with `GET /api/v1/health`, which needs no key and is sent
//! without one: it is the «server answers» probe, runs every time and gives the
//! server's version. Then:
//!
//! - every fetch: `GET /api/v1/system/playback/sessions` (streams, transcodes);
//! - every [`SLOW_EVERY`]th fetch: `GET /api/v1/libraries/health` and
//!   `GET /api/v1/system/tasks`. In between, the last values of those calls,
//!   and the error they had if one failed, are carried forward, so the numbers
//!   and the status do not flicker.
//!
//! The kind does not look at the server's capability list: the names the spec
//! leaves unnamed are not the names a real server (0.13.1) advertises. It
//! calls the endpoints, and a 404 or 501 means «this server does not have it»:
//! the address is then left alone for an hour ([`OldAddresses`]) and only that
//! call's numbers are missing.
//!
//! **Streams** are the sessions that are not paused; **transcodes** are the
//! sessions whose `decision_mode` contains "Transcode" (a server may copy the
//! video and transcode only the sound, which is still a transcode). Every task
//! the server lists is a running background task.
//!
//! **Libraries.** One with unavailable titles is an error (`tofa:library`,
//! bad). One with files that fail to probe, titles that are not matched or
//! files that were skipped, and nothing unavailable, is a library with issues
//! (`tofa:library_warn`, warning): most libraries have some unmatched titles,
//! so it is a nudge, not an alarm. Names are never kept.
//!
//! The key is sent as `Authorization: Bearer`, marked sensitive, and acts as
//! an admin in Tofa: it is a password. A 403 on an admin endpoint means the
//! key is known but is no admin key ([`FetchError::NotAdmin`]). Titles,
//! library names and user names from Tofa are never kept: the reading holds
//! numbers and the server's version only.
//!
//! What the kind remembers between fetches lives in memory, per address, in a
//! bounded list ([`MEMORY_MAX`]): a fetch counter and the last slow values.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value as Json;
use url::Url;

use super::api::{self, Credential, OldAddresses, Reply};
use super::{
    BoxFuture, FetchCtx, FetchError, Field, FieldKind, Format, Integration, Level, MetricDef, Need,
    Snapshot, Status, Value,
};

/// The only instance.
pub static TOFA: TofaKind = TofaKind;

/// See the module.
pub struct TofaKind;

/// The key of the need «the server does not answer».
pub const DOWN_NEED: &str = "tofa:down";
/// The key of the need «libraries with errors».
pub const LIBRARY_NEED: &str = "tofa:library";
/// The key of the need «libraries with issues».
pub const LIBRARY_WARN_NEED: &str = "tofa:library_warn";

/// Every how-many-th fetch refreshes the slow calls (the first one does).
pub const SLOW_EVERY: u64 = 4;
/// Most addresses the kind remembers; past that the one used longest ago goes.
pub const MEMORY_MAX: usize = 64;

const VERSION_CHARS: usize = 40;

/// The addresses of endpoints that answered 404 or 501.
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

static METRICS: [MetricDef; 5] = [
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
        id: "tasks_running",
        label_key: "metric.tofa_tasks_running",
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

const HEALTH: &[&str] = &["api", "v1", "health"];
const SESSIONS: &[&str] = &["api", "v1", "system", "playback", "sessions"];
const LIBRARIES: &[&str] = &["api", "v1", "libraries", "health"];
const TASKS: &[&str] = &["api", "v1", "system", "tasks"];

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
        Box::pin(read(ctx))
    }
}

async fn read(ctx: &FetchCtx) -> Result<Snapshot, FetchError> {
    let base = api::base_url(&ctx.settings)?;
    let key = Credential::bearer(ctx.secret.as_deref())?;
    let address = base.to_string();
    let before = remember(&address, |m| {
        let before = m.clone();
        m.fetches = m.fetches.wrapping_add(1);
        before
    });
    let slow_due = before.fetches % SLOW_EVERY == 0;

    // Is the server there? Any failure says no. The health call is public, so
    // it is sent without the key.
    let url = api::endpoint(&base, HEALTH, None)?;
    let health = match api::get_json_public::<Json>(&ctx.http, url).await {
        Ok(Reply::Found(doc)) => doc,
        Ok(Reply::NotFound) => return Ok(down(FetchError::Status(404))),
        Err(err) => return Ok(down(err)),
    };
    let version = text_of(&health, "version");

    let mut error = None;
    let sessions = call(ctx, &base, &key, SESSIONS)
        .await
        .and_then(|doc| doc.as_ref().map(read_sessions).transpose());
    let sessions = match got(sessions, &mut error)? {
        Got::Value(counts) => Some(counts),
        Got::Absent | Got::Failed => None,
    };

    let mut slow = before.slow;
    if slow_due {
        let (libraries, tasks) = tokio::join!(
            call(ctx, &base, &key, LIBRARIES),
            call(ctx, &base, &key, TASKS),
        );
        let libraries = libraries.and_then(|doc| doc.as_ref().map(read_libraries).transpose());
        let tasks = tasks.and_then(|doc| doc.as_ref().map(read_tasks).transpose());
        let mut slow_error = None;
        match got(libraries, &mut slow_error)? {
            Got::Value(counts) => slow.libraries = Some(counts),
            Got::Absent => slow.libraries = None,
            Got::Failed => {}
        }
        match got(tasks, &mut slow_error)? {
            Got::Value(n) => slow.tasks = Some(n),
            Got::Absent => slow.tasks = None,
            Got::Failed => {}
        }
        // The error goes with the values it belongs to: until the next
        // refresh the reading says the same as this one did.
        slow.error = slow_error;
        remember(&address, |m| m.slow = slow.clone());
    }

    Ok(reading(version, sessions, &slow, error.or(slow.error)))
}

// ---------------------------------------------------------------------------
// Memory

/// What is remembered of an address between fetches.
#[derive(Debug, Clone, Default)]
struct Memory {
    /// How many fetches began (it decides which one refreshes the slow calls).
    fetches: u64,
    slow: Slow,
}

/// The values of the slow calls, carried between their refreshes, and why a
/// call of the last refresh failed, if one did.
#[derive(Debug, Clone, Default, PartialEq)]
struct Slow {
    libraries: Option<Libraries>,
    tasks: Option<f64>,
    error: Option<FetchError>,
}

/// How many libraries have errors and how many only have issues.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Libraries {
    errors: f64,
    issues: f64,
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

/// One GET of a data endpoint (they are for admins). `None`: the address
/// answered 404 or 501 less than an hour ago, or does now (this server does not
/// have the endpoint).
async fn call(
    ctx: &FetchCtx,
    base: &Url,
    key: &Credential,
    path: &[&str],
) -> Result<Option<Json>, FetchError> {
    let url = api::endpoint(base, path, None)?;
    let old_key = url.to_string();
    if OLD.is_old(&old_key, Instant::now()) {
        return Ok(None);
    }
    match api::get_json_admin::<Json>(&ctx.http, url, key).await {
        Ok(Reply::Found(doc)) => {
            OLD.forget(&old_key);
            Ok(Some(doc))
        }
        Ok(Reply::NotFound) | Err(FetchError::Status(501)) => {
            OLD.mark(&old_key, Instant::now());
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// What came of one data call.
enum Got<T> {
    Value(T),
    /// The server has no such endpoint.
    Absent,
    /// It failed; the reason is in the error.
    Failed,
}

/// Sorts a call's result: a refused key, or one that is no admin key, ends the
/// fetch; any other failure is noted (the first one) and costs only that
/// call's numbers.
fn got<T>(
    result: Result<Option<T>, FetchError>,
    error: &mut Option<FetchError>,
) -> Result<Got<T>, FetchError> {
    match result {
        Ok(Some(value)) => Ok(Got::Value(value)),
        Ok(None) => Ok(Got::Absent),
        Err(err @ (FetchError::Auth | FetchError::NotAdmin)) => Err(err),
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

fn array(doc: &Json) -> Result<&Vec<Json>, FetchError> {
    doc.as_array().ok_or(FetchError::Invalid)
}

/// (streams, transcodes) of a sessions answer. A stream is a session that is
/// not paused; a transcode is a session whose `decision_mode` has "transcode"
/// in it (`HlsFullTranscode`, `HlsCopyVideoTranscodeAudio` …), paused or not.
fn read_sessions(doc: &Json) -> Result<(f64, f64), FetchError> {
    let list = array(doc)?;
    let streams = list
        .iter()
        .filter(|s| s.get("is_paused").and_then(Json::as_bool) != Some(true))
        .count();
    let transcodes = list
        .iter()
        .filter(|s| {
            s.get("decision_mode")
                .and_then(Json::as_str)
                .is_some_and(|mode| mode.to_ascii_lowercase().contains("transcode"))
        })
        .count();
    Ok((streams as f64, transcodes as f64))
}

fn positive(library: &Json, field: &str) -> bool {
    api::count(library.get(field)).is_some_and(|n| n > 0.0)
}

/// How many libraries have errors (unavailable titles) and how many of the
/// others have issues (files that fail to probe, unmatched titles, skipped
/// files).
fn read_libraries(doc: &Json) -> Result<Libraries, FetchError> {
    let (mut errors, mut issues) = (0.0, 0.0);
    for library in array(doc)? {
        if positive(library, "unavailable_titles") {
            errors += 1.0;
        } else if ["probe_failed_files", "unmatched_titles", "skipped_files"]
            .iter()
            .any(|field| positive(library, field))
        {
            issues += 1.0;
        }
    }
    Ok(Libraries { errors, issues })
}

/// How many background tasks the server lists; every one is running.
fn read_tasks(doc: &Json) -> Result<f64, FetchError> {
    Ok(array(doc)?.len() as f64)
}

// ---------------------------------------------------------------------------
// The reading

/// A need whose only parameter is how many.
fn counted_need(key: &str, level: Level, label_key: &str, count: f64) -> Need {
    Need {
        key: key.to_string(),
        level,
        label_key: label_key.to_string(),
        params: [("count".to_string(), format!("{count:.0}"))].into(),
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

/// The reading of a server that answers: bad when a library has errors, warn
/// when one has issues or a call failed, ok otherwise.
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
    put("tasks_running", slow.tasks);
    put("libraries_unhealthy", slow.libraries.map(|l| l.errors));
    if let Some(version) = version {
        snapshot
            .values
            .insert("version".to_string(), Value::text(version));
    }

    let Libraries { errors, issues } = slow.libraries.unwrap_or(Libraries {
        errors: 0.0,
        issues: 0.0,
    });
    if errors > 0.0 {
        snapshot.needs.push(counted_need(
            LIBRARY_NEED,
            Level::Bad,
            "need.tofa_library",
            errors,
        ));
        snapshot.status = Status::Bad;
    }
    if issues > 0.0 {
        snapshot.needs.push(counted_need(
            LIBRARY_WARN_NEED,
            Level::Warn,
            "need.tofa_library_warn",
            issues,
        ));
        if snapshot.status == Status::Ok {
            snapshot.status = Status::Warn;
        }
    }
    if error.is_some() && snapshot.status == Status::Ok {
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
    fn the_version_is_cut_and_cleaned() {
        let long = "9".repeat(200);
        let text = text_of(
            &json!({ "version": format!("0.13.1\u{202E}{long}") }),
            "version",
        )
        .unwrap();
        assert_eq!(text.chars().count(), 40);
        assert!(!text.contains('\u{202E}'));
        assert_eq!(text_of(&json!({ "version": "  " }), "version"), None);
        assert_eq!(text_of(&json!({ "version": 5 }), "version"), None);
    }

    #[test]
    fn a_stream_is_not_paused_and_a_transcode_has_the_word_in_its_mode() {
        let doc = json!([
            { "is_paused": false, "decision_mode": "DirectFile" },
            { "is_paused": false, "decision_mode": "HlsCopyVideoTranscodeAudio" },
            { "is_paused": true, "decision_mode": "HlsFullTranscode" },
            { "decision_mode": "hlsfulltranscode" },
            { "is_paused": "yes" },
            {},
        ]);
        // Not paused: the 1st, 2nd, 4th, 5th and 6th.
        assert_eq!(read_sessions(&doc).unwrap(), (5.0, 3.0));
        assert_eq!(read_sessions(&json!([])).unwrap(), (0.0, 0.0));
        assert_eq!(read_sessions(&json!({})), Err(FetchError::Invalid));
    }

    #[test]
    fn a_library_has_errors_or_issues_never_both() {
        let doc = json!([
            { "unavailable_titles": 2, "probe_failed_files": 5 },
            { "unavailable_titles": 0, "probe_failed_files": 2 },
            { "unmatched_titles": 12 },
            { "skipped_files": 1 },
            { "unavailable_titles": 0, "unmatched_titles": 0, "skipped_files": 0 },
            { "unavailable_titles": -1, "probe_failed_files": "x" },
            {},
            "odd",
        ]);
        assert_eq!(
            read_libraries(&doc).unwrap(),
            Libraries {
                errors: 1.0,
                issues: 3.0
            }
        );
        assert_eq!(read_libraries(&json!(5)), Err(FetchError::Invalid));
    }

    #[test]
    fn every_listed_task_is_running() {
        let doc = json!([{ "status": "running" }, { "status": "failed" }, {}]);
        assert_eq!(read_tasks(&doc).unwrap(), 3.0);
        assert_eq!(read_tasks(&json!([])).unwrap(), 0.0);
        assert_eq!(read_tasks(&json!({})), Err(FetchError::Invalid));
    }

    #[test]
    fn the_reading_carries_the_slow_error_with_the_slow_values() {
        let slow = Slow {
            libraries: Some(Libraries {
                errors: 0.0,
                issues: 0.0,
            }),
            tasks: Some(1.0),
            error: None,
        };
        let ok = reading(None, Some((1.0, 0.0)), &slow, None);
        assert_eq!(ok.status, Status::Ok);
        let failed = reading(None, Some((1.0, 0.0)), &slow, Some(FetchError::Timeout));
        assert_eq!(failed.status, Status::Warn);
        assert_eq!(failed.error.as_deref(), Some("timeout"));
    }
}
