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
//! the address is then left alone for a few minutes ([`api::OLD_FOR`], by
//! [`OldAddresses`]) and only that
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
//! so it is a nudge, not an alarm.
//!
//! **Details.** The reading carries blocks for the card: what is playing (a
//! list, one line per session: title, «user (profile) · client», a
//! «Transcoding» badge for a transcode, and warning while paused), a bar block
//! with each session's progress, the libraries
//! (worst first, with the episode count) and facts (background tasks, server
//! version). Titles, library names and user and profile names go into these
//! blocks only, as text, never into the numbers, the needs or an error. They
//! are shown to everyone who may see the service. The user is the session's
//! `username`; the profile name is added in brackets when it is another word
//! (it is often only a default label).
//! The libraries' lines are carried between the slow refreshes with the rest.
//!
//! The key is sent as `Authorization: Bearer`, marked sensitive, and acts as
//! an admin in Tofa: it is a password. A 403 on an admin endpoint means the
//! key is known but is no admin key ([`FetchError::NotAdmin`]).
//!
//! What the kind remembers between fetches lives in memory, per address, in a
//! bounded list ([`MEMORY_MAX`]): a fetch counter and the last slow values.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value as Json;
use url::Url;

use super::api::{self, Credential, OldAddresses, Reply};
use super::block::{Bar, Block, Fact, ListItem, MAX_ITEMS, TEXT_CHARS};
use super::{
    BoxFuture, FetchCtx, FetchError, Field, FieldKind, Format, Integration, Level, MetricDef, Need,
    Snapshot, Status, Value,
};

/// The only instance.
pub static TOFA: TofaKind = TofaKind;

/// See the module.
pub struct TofaKind;

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

    fn default_size(&self) -> crate::layout::Size {
        crate::layout::Size::Wide
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
    let address = api::service_prefix(&base);
    let before = remember(&address, |m| {
        let before = m.clone();
        m.fetches = m.fetches.wrapping_add(1);
        before
    });
    let slow_due = before.fetches % SLOW_EVERY == 0;

    // Is the server there? Any failure says no: an error, and the poller makes
    // «not answering» of it when it repeats (once is not worth a need). The
    // health call is public, so it is sent without the key.
    let url = api::endpoint(&base, HEALTH, None)?;
    let health = api::get_json_public::<Json>(&ctx.http, url)
        .await?
        .found()?;
    let version = text_of(&health, "version");

    let mut error = None;
    let sessions = call(ctx, &base, &key, SESSIONS).await.and_then(|doc| {
        doc.as_ref()
            .map(|doc| Ok((read_sessions(doc)?, read_plays(doc))))
            .transpose()
    });
    let (sessions, plays) = match got(sessions, &mut error)? {
        Got::Value((counts, plays)) => (Some(counts), plays),
        Got::Absent | Got::Failed => (None, Vec::new()),
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

    let details = details(version.as_deref(), &plays, &slow);
    Ok(reading(version, sessions, &slow, error.or(slow.error)).with_details(details))
}

// ---------------------------------------------------------------------------
// Memory

/// What is remembered of an address between fetches (the key is
/// [`api::service_prefix`]).
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

/// How many libraries have errors and how many only have issues, and the
/// lines of the libraries list (the worst first, at most [`MAX_ITEMS`]).
#[derive(Debug, Clone, Default, PartialEq)]
struct Libraries {
    errors: f64,
    issues: f64,
    rows: Vec<LibraryRow>,
}

/// One library of the list.
#[derive(Debug, Clone, PartialEq)]
struct LibraryRow {
    name: String,
    episodes: Option<u64>,
    status: Status,
}

/// One session of the now-playing list.
#[derive(Debug, Clone, PartialEq)]
struct Play {
    title: String,
    /// «user (profile) · client».
    sub: Option<String>,
    /// Whether the server transcodes it (the line's «Transcoding» badge).
    transcode: bool,
    /// Whole percent played, when the session says where it is and how long
    /// the media is.
    progress: Option<u32>,
    paused: bool,
}

/// Runs `f` on the memory of `key` (made if it is not there), which then
/// counts as the most recently used. The list holds at most [`MEMORY_MAX`]
/// addresses.
fn remember<R>(key: &str, f: impl FnOnce(&mut Memory) -> R) -> R {
    // Saving or testing the service forgets what is remembered of it.
    api::on_forget(forget);
    // A panic elsewhere cannot leave the list in a state that matters.
    let mut list = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    remember_in(&mut list, key, f)
}

/// Forgets the memory of the service at `prefix` (see [`api::service_prefix`]):
/// the next fetch counts as the first, refreshes the slow calls and drops what
/// was carried from the last ones.
fn forget(prefix: &str) {
    MEMORY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(key, _)| key != prefix);
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
/// answered 404 or 501 less than [`api::OLD_FOR`] ago, or does now (this server does not
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
    let mut rows = Vec::new();
    for library in array(doc)? {
        let status = if positive(library, "unavailable_titles") {
            errors += 1.0;
            Status::Bad
        } else if ["probe_failed_files", "unmatched_titles", "skipped_files"]
            .iter()
            .any(|field| positive(library, field))
        {
            issues += 1.0;
            Status::Warn
        } else {
            Status::Ok
        };
        if let Some(name) = line_of(library, "name") {
            rows.push(LibraryRow {
                name,
                episodes: api::count(library.get("episodes_total"))
                    .filter(|n| *n > 0.0)
                    .map(|n| n as u64),
                status,
            });
        }
    }
    // The worst first; the order the server gave otherwise.
    rows.sort_by_key(|row| match row.status {
        Status::Bad => 0,
        Status::Warn => 1,
        _ => 2,
    });
    rows.truncate(MAX_ITEMS);
    Ok(Libraries {
        errors,
        issues,
        rows,
    })
}

/// A line of text of a session or library from outside: one line, cut.
fn line_of(doc: &Json, field: &str) -> Option<String> {
    doc.get(field)
        .and_then(Json::as_str)
        .map(|text| api::clean_text(text, TEXT_CHARS))
        .filter(|text| !text.is_empty())
}

/// Who plays: the user, with the profile in brackets when it is another word
/// than the user's name; the profile alone when there is no user name.
fn who(user: Option<String>, profile: Option<String>) -> Option<String> {
    match (user, profile) {
        (Some(user), Some(profile)) if !profile.eq_ignore_ascii_case(&user) => {
            Some(format!("{user} ({profile})"))
        }
        (Some(user), _) => Some(user),
        (None, profile) => profile,
    }
}

/// The sessions of the now-playing list: those with a title, at most
/// [`MAX_ITEMS`]. Anything else in the answer is not looked at.
fn read_plays(doc: &Json) -> Vec<Play> {
    doc.as_array()
        .into_iter()
        .flatten()
        .filter_map(|session| {
            let title = line_of(session, "media_title")?;
            let transcode = session
                .get("decision_mode")
                .and_then(Json::as_str)
                .is_some_and(|mode| mode.to_ascii_lowercase().contains("transcode"));
            let parts: Vec<String> = [
                who(
                    line_of(session, "username"),
                    line_of(session, "profile_name"),
                ),
                line_of(session, "client_name"),
            ]
            .into_iter()
            .flatten()
            .collect();
            let position = api::count(session.get("position_ms"));
            let duration = api::count(session.get("duration_ms")).filter(|d| *d > 0.0);
            Some(Play {
                title,
                sub: (!parts.is_empty()).then(|| parts.join(" \u{b7} ")),
                transcode,
                progress: position
                    .zip(duration)
                    .map(|(at, of)| (at / of * 100.0).round().clamp(0.0, 100.0) as u32),
                paused: session.get("is_paused").and_then(Json::as_bool) == Some(true),
            })
        })
        .take(MAX_ITEMS)
        .collect()
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
    put(
        "libraries_unhealthy",
        slow.libraries.as_ref().map(|l| l.errors),
    );
    if let Some(version) = version {
        snapshot
            .values
            .insert("version".to_string(), Value::text(version));
    }

    let (errors, issues) = slow
        .libraries
        .as_ref()
        .map_or((0.0, 0.0), |l| (l.errors, l.issues));
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

/// An integer with a space between its thousands («6 576»).
fn spaced(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(digit);
    }
    out
}

/// The blocks of the card: what is playing, the libraries, and the facts of
/// the server. Each is left out when there is nothing to put in it.
fn details(version: Option<&str>, plays: &[Play], slow: &Slow) -> Vec<Block> {
    let mut blocks = Vec::new();
    if !plays.is_empty() {
        blocks.push(Block::List {
            title_key: "tofa_playing".to_string(),
            items: plays
                .iter()
                .map(|play| ListItem {
                    text: play.title.clone(),
                    sub: play.sub.clone(),
                    status: Some(if play.paused {
                        Status::Warn
                    } else {
                        Status::Ok
                    }),
                    right: None,
                    href: None,
                    badge_key: play.transcode.then(|| "tofa_transcode".to_string()),
                    ..Default::default()
                })
                .collect(),
        });
        // The progress is drawn as bars, in the order of the list (so the
        // list has no progress text of its own).
        let bars: Vec<Bar> = plays
            .iter()
            .filter_map(|play| {
                let pct = play.progress?;
                Some(Bar {
                    label: play.title.clone(),
                    pct: f64::from(pct),
                    text: format!("{pct} %"),
                    ..Default::default()
                })
            })
            .collect();
        if !bars.is_empty() {
            blocks.push(Block::Bars {
                title_key: "tofa_progress".to_string(),
                bars,
            });
        }
    }
    if let Some(libraries) = slow.libraries.as_ref().filter(|l| !l.rows.is_empty()) {
        blocks.push(Block::List {
            title_key: "tofa_libraries".to_string(),
            items: libraries
                .rows
                .iter()
                .map(|row| ListItem {
                    text: row.name.clone(),
                    sub: row.episodes.map(|n| format!("{} ep.", spaced(n))),
                    status: Some(row.status),
                    right: None,
                    href: None,
                    ..Default::default()
                })
                .collect(),
        });
    }
    let mut rows = Vec::new();
    if let Some(tasks) = slow.tasks {
        rows.push(Fact {
            label_key: Some("tofa_tasks".to_string()),
            label: None,
            value: format!("{tasks:.0}"),
            ..Default::default()
        });
    }
    if let Some(version) = version {
        rows.push(Fact {
            label_key: Some("tofa_version".to_string()),
            label: None,
            value: version.to_string(),
            ..Default::default()
        });
    }
    if !rows.is_empty() {
        blocks.push(Block::Facts {
            title_key: "tofa_server".to_string(),
            rows,
        });
    }
    blocks
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
        let libraries = read_libraries(&doc).unwrap();
        assert_eq!((libraries.errors, libraries.issues), (1.0, 3.0));
        assert_eq!(read_libraries(&json!(5)), Err(FetchError::Invalid));
    }

    #[test]
    fn thousands_are_spaced() {
        for (n, text) in [
            (0, "0"),
            (12, "12"),
            (999, "999"),
            (1000, "1 000"),
            (6576, "6 576"),
            (1_234_567, "1 234 567"),
        ] {
            assert_eq!(spaced(n), text);
        }
    }

    #[test]
    fn the_user_is_shown_with_the_profile_only_when_it_is_another_word() {
        let word = |s: &str| Some(s.to_string());
        assert_eq!(
            who(word("kari"), word("Main")).as_deref(),
            Some("kari (Main)")
        );
        assert_eq!(who(word("kari"), word("KARI")).as_deref(), Some("kari"));
        assert_eq!(who(word("kari"), None).as_deref(), Some("kari"));
        assert_eq!(who(None, word("Main")).as_deref(), Some("Main"));
        assert_eq!(who(None, None), None);
        let doc = json!([{ "media_title": "A", "username": "ola", "profile_name": "Ola" }]);
        assert_eq!(read_plays(&doc)[0].sub.as_deref(), Some("ola"));
    }

    #[test]
    fn a_session_without_a_title_is_not_listed_and_the_list_is_cut() {
        let doc = json!([
            { "media_title": "A", "profile_name": "p" },
            { "profile_name": "no title" },
            { "media_title": "  " },
            "odd",
        ]);
        let plays = read_plays(&doc);
        assert_eq!(plays.len(), 1);
        assert_eq!(plays[0].sub.as_deref(), Some("p"));
        assert_eq!(plays[0].progress, None);
        let many = json!(
            (0..30)
                .map(|i| json!({ "media_title": format!("t{i}") }))
                .collect::<Vec<_>>()
        );
        assert_eq!(read_plays(&many).len(), MAX_ITEMS);
        assert!(read_plays(&json!({})).is_empty());
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
            libraries: Some(Libraries::default()),
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
