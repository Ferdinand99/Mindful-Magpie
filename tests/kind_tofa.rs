//! The `tofa` kind («Tofa») against a stub of Tofa's API, with answers shaped
//! like those of a real 0.13.1 server: the numbers, the status and the needs,
//! the public health call (sent without the key), endpoints a server does not
//! have, the staged refresh (fast calls every fetch, slow calls every fourth,
//! with their error carried along), what a refused key, a key that is no admin
//! key, a failing call or a malformed answer does, and that the key stays out
//! of every error, reading and log.

mod common;

use std::time::Duration;

use common::stub::{self, Answer, Stub};
use common::{locale, log};
use mindful_magpie::integrations::tofa::{LIBRARY_NEED, LIBRARY_WARN_NEED, SLOW_EVERY};
use mindful_magpie::integrations::{
    self, FetchCtx, FetchError, Format, Level, Snapshot, Status, error_text_key,
};
use serde_json::{Value as Json, json};

const KEY: &str = "tofa_s3cr3t-K3y-9f8e7d6c";

const HEALTH: &str = "/api/v1/health";
const SESSIONS: &str = "/api/v1/system/playback/sessions";
const LIBRARIES: &str = "/api/v1/libraries/health";
const TASKS: &str = "/api/v1/system/tasks";
/// Endpoints the kind must never call: the capability list, and a feed that a
/// key may not read.
const NEVER: [&str; 2] = ["/api/v1/system/info", "/api/v1/media/changes"];

// ---------------------------------------------------------------------------
// Answers in the shapes of a real server

fn health() -> Json {
    json!({
        "status": "ok",
        "database": "ok",
        "version": "0.13.1",
        "server_id": "0f6c5d5a-7d0e-4a56-8d58-6a1f8f0c2b11",
    })
}

/// One session per `(play_method, decision_mode, paused)`.
fn sessions(list: &[(&str, &str, bool)]) -> Json {
    json!(
        list.iter()
            .enumerate()
            .map(|(i, (method, mode, paused))| json!({
                "session_id": format!("00000000-0000-4000-8000-00000000000{i}"),
                "username": format!("user{i}"),
                "media_title": format!("Film {i}"),
                "client_name": "Tofa Web",
                "is_paused": paused,
                "play_method": method,
                "decision_mode": mode,
                "transcode_reason_codes": [],
            }))
            .collect::<Vec<_>>()
    )
}

/// A library as `libraries/health` lists it; everything fine unless told.
fn library(name: &str, unavailable: i64, probe_failed: i64, unmatched: i64, skipped: i64) -> Json {
    json!({
        "library_id": "44444444-4444-4444-8444-444444444444",
        "name": name,
        "unmatched_titles": unmatched,
        "match_pending_titles": 0,
        "unavailable_titles": unavailable,
        "skipped_files": skipped,
        "probe_failed_files": probe_failed,
        "unbound_episode_files": 0,
        "episodes_flagged": 0,
        "folder_overlaps": [],
    })
}

fn clean_library() -> Json {
    library("Filmer", 0, 0, 0, 0)
}

fn tasks(n: usize) -> Json {
    json!(
        (0..n)
            .map(|i| json!({
                "id": format!("66666666-6666-4666-8666-66666666666{i}"),
                "status": "whatever",
                "progress": 0.4,
            }))
            .collect::<Vec<_>>()
    )
}

/// A quiet Tofa: three sessions (one transcodes the sound only, one is
/// paused), nothing wrong, one background task.
fn quiet(stub: &Stub) {
    stub.ok(HEALTH, &health());
    stub.ok(
        SESSIONS,
        &sessions(&[
            ("DirectStream", "HlsCopyVideoTranscodeAudio", false),
            ("DirectPlay", "DirectFile", false),
            ("DirectPlay", "DirectFile", true),
        ]),
    );
    stub.ok(LIBRARIES, &json!([clean_library()]));
    stub.ok(TASKS, &tasks(1));
}

fn ctx(base: &str, secret: Option<&str>) -> FetchCtx {
    FetchCtx {
        http: integrations::build_client(Duration::from_secs(5)),
        settings: json!({ "url": base }),
        secret: secret.map(str::to_string),
    }
}

fn tofa() -> &'static dyn integrations::Integration {
    integrations::find("tofa").expect("the registry lists tofa")
}

/// A fetch with the key that is expected to give a reading, good or bad.
async fn fetch(base: &str) -> Snapshot {
    tofa()
        .fetch(&ctx(base, Some(KEY)))
        .await
        .expect("a reading, good or bad")
}

/// A fetch with the key that is expected to give none: the reason.
async fn fetch_err(base: &str) -> FetchError {
    tofa()
        .fetch(&ctx(base, Some(KEY)))
        .await
        .expect_err("no reading at all")
}

fn num(snapshot: &Snapshot, id: &str) -> Option<f64> {
    snapshot.values.get(id).and_then(|v| v.num)
}

fn text(snapshot: &Snapshot, id: &str) -> Option<String> {
    snapshot.values.get(id).and_then(|v| v.text.clone())
}

fn need_keys(snapshot: &Snapshot) -> Vec<&str> {
    snapshot.needs.iter().map(|n| n.key.as_str()).collect()
}

// ---------------------------------------------------------------------------
// The kind

#[test]
fn the_kind_describes_itself() {
    let kind = tofa();
    assert_eq!(kind.kind(), "tofa");
    assert_eq!(kind.label_key(), "kind.tofa");
    assert_eq!(kind.default_interval(), Duration::from_secs(15));
    assert_eq!(kind.min_interval(), Duration::from_secs(15));
    assert!(kind.url_required());
    let metrics: Vec<_> = kind.metrics().iter().map(|m| (m.id, m.format)).collect();
    assert_eq!(
        metrics,
        [
            ("streams", Format::Number),
            ("transcodes", Format::Number),
            ("tasks_running", Format::Number),
            ("libraries_unhealthy", Format::Number),
            ("version", Format::Text),
        ]
    );
    // The key is the only field, and it is the secret; no fingerprint yet.
    let fields = kind.fields();
    assert_eq!(fields.len(), 1);
    assert!(fields[0].secret && fields[0].required);
    assert_eq!(fields[0].key, "secret");
    assert_eq!(fields[0].label_key, "field.tofa_secret");
    assert_eq!(fields[0].help_key, "field.tofa_secret_help");
    assert_eq!(SLOW_EVERY, 4);
}

#[test]
fn every_text_of_the_kind_exists_in_both_languages() {
    let kind = tofa();
    let mut keys = vec![kind.label_key().to_string()];
    for field in kind.fields() {
        keys.push(field.label_key.to_string());
        keys.push(field.help_key.to_string());
    }
    keys.extend(kind.metrics().iter().map(|m| m.label_key.to_string()));
    keys.extend(
        [
            "need.tofa_library",
            "need.tofa_library_warn",
            "fetch_err.not_admin",
            "fetch_err.auth",
        ]
        .map(str::to_string),
    );
    for key in &keys {
        locale::in_both(key).unwrap();
    }
}

#[test]
fn the_texts_say_what_was_agreed() {
    let (en, nb) = (locale::LOCALES[0].1, locale::LOCALES[1].1);
    let t = |file, section, name| locale::text(file, section, name).unwrap();
    assert_eq!(
        t(nb, "need", "tofa_library_warn"),
        "Bibliotek med avvik: {count}"
    );
    assert_eq!(
        t(en, "need", "tofa_library_warn"),
        "Libraries with issues: {count}"
    );
    assert_eq!(t(nb, "need", "tofa_library"), "Bibliotek med feil: {count}");
    assert_eq!(t(nb, "metric", "tofa_tasks_running"), "Bakgrunnsjobber");
    assert_eq!(t(en, "metric", "tofa_tasks_running"), "Background tasks");
    assert_eq!(
        t(nb, "fetch_err", "not_admin"),
        "Nøkkelen er ikke en admin-nøkkel"
    );
    assert_eq!(
        t(en, "fetch_err", "not_admin"),
        "The key is not an admin key"
    );
    // Noun, a colon, the count; the server's own «down» has no count.
    for file in [en, nb] {
        for name in ["tofa_library", "tofa_library_warn"] {
            let label = t(file, "need", name);
            let noun = label.strip_suffix(": {count}").expect(&label);
            assert!(
                !noun.is_empty() && !noun.contains(['{', '}', ':']),
                "{label}"
            );
        }
        // The generic «not answering» replaced Tofa's own.
        assert!(locale::text(file, "need", "tofa_down").is_none());
        // What was dropped is gone.
        assert!(locale::text(file, "metric", "tofa_new_items").is_none());
        assert!(locale::text(file, "metric", "tofa_scans_running").is_none());
    }
    let help = t(en, "field", "tofa_secret_help");
    assert!(help.contains("0.9.34") && help.contains("admin"), "{help}");
}

#[test]
fn the_refused_key_texts_map_to_their_translation_keys() {
    assert_eq!(
        error_text_key(&FetchError::Auth.to_string()),
        Some("fetch_err.auth")
    );
    assert_eq!(
        error_text_key(&FetchError::NotAdmin.to_string()),
        Some("fetch_err.not_admin")
    );
}

// ---------------------------------------------------------------------------
// Readings

#[tokio::test]
async fn a_quiet_tofa_gives_every_number_and_is_ok() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok);
    assert!(s.needs.is_empty(), "{:?}", s.needs);
    assert_eq!(s.error, None);
    assert_eq!(
        num(&s, "streams"),
        Some(2.0),
        "the paused one is not a stream"
    );
    assert_eq!(num(&s, "transcodes"), Some(1.0));
    assert_eq!(num(&s, "tasks_running"), Some(1.0));
    assert_eq!(num(&s, "libraries_unhealthy"), Some(0.0));
    assert_eq!(text(&s, "version").as_deref(), Some("0.13.1"));
}

#[tokio::test]
async fn the_first_fetch_calls_what_it_needs_and_nothing_else() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    fetch(&base).await;
    assert_eq!(stub.seen().len(), 4, "{:?}", stub.seen());
    for path in [HEALTH, SESSIONS, LIBRARIES, TASKS] {
        assert_eq!(stub.count(path), 1, "{path}");
    }
    for path in NEVER {
        assert_eq!(stub.count(path), 0, "{path} is never called");
    }
    // The public health call carries no key; every other call does.
    let health = &stub.hits_on(HEALTH)[0];
    assert_eq!(health.authorization, None);
    assert_eq!(health.api_key, None);
    for path in [SESSIONS, LIBRARIES, TASKS] {
        assert_eq!(
            stub.hits_on(path)[0].authorization.as_deref(),
            Some(format!("Bearer {KEY}").as_str()),
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_real_servers_libraries_with_a_failed_probe_give_a_warning_not_an_alarm() {
    // "Filmer" with two files that fail to probe, as the real server had.
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(LIBRARIES, &json!([library("Filmer", 0, 2, 0, 0)]));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Warn);
    assert_eq!(need_keys(&s), [LIBRARY_WARN_NEED]);
    let n = &s.needs[0];
    assert_eq!(n.level, Level::Warn);
    assert_eq!(n.label_key, "need.tofa_library_warn");
    assert_eq!(n.params.get("count").map(String::as_str), Some("1"));
    assert_eq!(n.score, 1.0);
    assert_eq!(num(&s, "libraries_unhealthy"), Some(0.0));
    assert_eq!(s.error, None);
}

#[tokio::test]
async fn unmatched_titles_and_skipped_files_warn_too() {
    for lib in [library("A", 0, 0, 12, 0), library("A", 0, 0, 0, 3)] {
        let (base, stub) = stub::serve().await;
        quiet(&stub);
        stub.ok(LIBRARIES, &json!([lib, clean_library()]));
        let s = fetch(&base).await;
        assert_eq!(s.status, Status::Warn);
        assert_eq!(need_keys(&s), [LIBRARY_WARN_NEED]);
    }
}

#[tokio::test]
async fn unavailable_titles_are_an_error_counted_per_library() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(
        LIBRARIES,
        &json!([
            library("A", 3, 0, 0, 0),
            library("B", 1, 9, 9, 9),
            library("C", 0, 1, 0, 0),
            clean_library(),
        ]),
    );
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(num(&s, "libraries_unhealthy"), Some(2.0));
    assert_eq!(need_keys(&s), [LIBRARY_NEED, LIBRARY_WARN_NEED]);
    let bad = &s.needs[0];
    assert_eq!(bad.level, Level::Bad);
    assert_eq!(bad.label_key, "need.tofa_library");
    assert_eq!(bad.params.get("count").map(String::as_str), Some("2"));
    assert_eq!(bad.score, 2.0);
    // B has errors, so it is not also a library with issues: only C is.
    let warn = &s.needs[1];
    assert_eq!(warn.level, Level::Warn);
    assert_eq!(warn.params.get("count").map(String::as_str), Some("1"));
}

#[tokio::test]
async fn a_session_is_counted_by_its_pause_and_its_mode() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(
        SESSIONS,
        &sessions(&[
            ("Transcode", "HlsFullTranscode", false),
            ("DirectStream", "HlsCopyVideoTranscodeAudio", true),
            ("DirectPlay", "DirectFile", false),
            ("DirectStream", "HlsRemux", false),
        ]),
    );
    let s = fetch(&base).await;
    assert_eq!(num(&s, "streams"), Some(3.0));
    assert_eq!(num(&s, "transcodes"), Some(2.0), "paused or not");
    stub.ok(SESSIONS, &json!([]));
    let s = fetch(&base).await;
    assert_eq!(num(&s, "streams"), Some(0.0));
    assert_eq!(num(&s, "transcodes"), Some(0.0));
}

#[tokio::test]
async fn every_listed_task_is_a_running_background_task() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(TASKS, &tasks(3));
    let s = fetch(&base).await;
    assert_eq!(num(&s, "tasks_running"), Some(3.0));
    assert_eq!(s.status, Status::Ok, "a task is nothing to worry about");
}

#[tokio::test]
async fn the_address_may_carry_a_path_prefix() {
    let (base, stub) = stub::serve().await;
    for (path, body) in [
        (HEALTH, health()),
        (SESSIONS, json!([])),
        (LIBRARIES, json!([])),
        (TASKS, json!([])),
    ] {
        stub.ok(&format!("/tofa{path}"), &body);
    }
    let s = fetch(&format!("{base}/tofa/")).await;
    assert_eq!(s.status, Status::Ok);
    assert_eq!(text(&s, "version").as_deref(), Some("0.13.1"));
}

// ---------------------------------------------------------------------------
// The staged refresh

#[tokio::test]
async fn between_the_slow_refreshes_only_the_fast_calls_run_and_slow_values_are_carried() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(LIBRARIES, &json!([library("A", 2, 0, 0, 0)]));
    stub.ok(TASKS, &tasks(2));
    let first = fetch(&base).await;
    assert_eq!(num(&first, "libraries_unhealthy"), Some(1.0));
    assert_eq!(num(&first, "tasks_running"), Some(2.0));
    // The slow calls would now answer something else, but are not asked.
    stub.ok(LIBRARIES, &json!([clean_library()]));
    stub.ok(TASKS, &tasks(0));
    stub.ok(SESSIONS, &sessions(&[("DirectPlay", "DirectFile", false)]));
    for _ in 1..SLOW_EVERY {
        let s = fetch(&base).await;
        assert_eq!(num(&s, "streams"), Some(1.0), "the fast call is fresh");
        assert_eq!(num(&s, "libraries_unhealthy"), Some(1.0), "carried");
        assert_eq!(num(&s, "tasks_running"), Some(2.0), "carried");
        assert_eq!(s.status, Status::Bad);
    }
    assert_eq!(stub.count(LIBRARIES), 1);
    assert_eq!(stub.count(TASKS), 1);
    assert_eq!(stub.count(HEALTH), SLOW_EVERY as usize);
    assert_eq!(stub.count(SESSIONS), SLOW_EVERY as usize);
    // The next one refreshes them.
    let s = fetch(&base).await;
    assert_eq!(num(&s, "libraries_unhealthy"), Some(0.0));
    assert_eq!(num(&s, "tasks_running"), Some(0.0));
    assert_eq!(s.status, Status::Ok);
    assert_eq!(stub.count(LIBRARIES), 2);
}

#[tokio::test]
async fn a_failing_slow_call_keeps_the_carried_value_and_its_error_until_the_next_refresh() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(TASKS, &tasks(2));
    let first = fetch(&base).await;
    assert_eq!(first.status, Status::Ok);
    // Skip to the next refresh, with the libraries call failing.
    for _ in 1..SLOW_EVERY {
        assert_eq!(fetch(&base).await.status, Status::Ok);
    }
    stub.route(LIBRARIES, Answer::Status(500));
    let failing = fetch(&base).await;
    assert_eq!(failing.status, Status::Warn);
    assert_eq!(failing.error.as_deref(), Some("HTTP 500"));
    assert_eq!(
        num(&failing, "tasks_running"),
        Some(2.0),
        "that call is fine"
    );
    assert_eq!(num(&failing, "libraries_unhealthy"), Some(0.0), "carried");
    // Between refreshes the card does not flip back to Ok: the warning and its
    // reason are carried with the values.
    stub.route(LIBRARIES, Answer::Json(json!([clean_library()])));
    for _ in 1..SLOW_EVERY {
        let s = fetch(&base).await;
        assert_eq!(s.status, Status::Warn);
        assert_eq!(s.error.as_deref(), Some("HTTP 500"));
    }
    // The refresh that works clears it.
    let healed = fetch(&base).await;
    assert_eq!(healed.status, Status::Ok);
    assert_eq!(healed.error, None);
}

#[tokio::test]
async fn services_at_other_addresses_have_their_own_rhythm_and_memory() {
    let (a, stub_a) = stub::serve().await;
    let (b, stub_b) = stub::serve().await;
    for stub in [&stub_a, &stub_b] {
        quiet(stub);
    }
    stub_a.ok(LIBRARIES, &json!([library("A", 1, 0, 0, 0)]));
    assert_eq!(num(&fetch(&a).await, "libraries_unhealthy"), Some(1.0));
    // B's first fetch refreshes its slow calls even though A has run once.
    assert_eq!(num(&fetch(&b).await, "libraries_unhealthy"), Some(0.0));
    assert_eq!(stub_b.count(LIBRARIES), 1);
}

// ---------------------------------------------------------------------------
// Endpoints a server does not have

#[tokio::test]
async fn an_endpoint_that_answers_404_or_501_is_left_alone_for_a_while() {
    for gone in [404, 501] {
        let (base, stub) = stub::serve().await;
        quiet(&stub);
        stub.route(LIBRARIES, Answer::Status(gone));
        let s = fetch(&base).await;
        // Only that call's numbers are missing: no error, no warning.
        assert_eq!(s.status, Status::Ok, "{gone}");
        assert_eq!(s.error, None, "{gone}");
        assert!(!s.values.contains_key("libraries_unhealthy"), "{gone}");
        assert_eq!(num(&s, "tasks_running"), Some(1.0), "{gone}");
        assert_eq!(num(&s, "streams"), Some(2.0), "{gone}");
        for _ in 0..SLOW_EVERY {
            fetch(&base).await;
        }
        assert_eq!(
            stub.count(LIBRARIES),
            1,
            "{gone}: asked once, then left alone"
        );
    }
}

#[tokio::test]
async fn an_unsupported_fast_endpoint_costs_only_its_numbers() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.route(SESSIONS, Answer::Status(501));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok);
    assert!(!s.values.contains_key("streams"));
    assert!(!s.values.contains_key("transcodes"));
    assert_eq!(num(&s, "tasks_running"), Some(1.0));
}

// ---------------------------------------------------------------------------
// Down

#[tokio::test]
async fn a_server_that_does_not_answer_gives_no_reading_and_asks_nothing_else() {
    // An error, not a bad reading with a need of its own: the poller makes
    // «not answering» of it when it repeats, so once is not an alarm.
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.route(HEALTH, Answer::Status(503));
    assert_eq!(fetch_err(&base).await, FetchError::Status(503));
    assert_eq!(stub.seen().len(), 1);
}

#[tokio::test]
async fn an_address_that_cannot_be_reached_or_is_not_tofa_is_down_too() {
    let free = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    assert_eq!(
        fetch_err(&format!("http://{free}")).await,
        FetchError::Connect
    );

    let (base, _stub) = stub::serve().await;
    // A stub with no health endpoint.
    assert_eq!(fetch_err(&base).await, FetchError::Status(404));
}

#[tokio::test]
async fn a_server_that_comes_back_is_fine_again() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.route(HEALTH, Answer::Status(503));
    assert_eq!(fetch_err(&base).await, FetchError::Status(503));
    stub.ok(HEALTH, &health());
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok);
    assert!(s.needs.is_empty());
}

#[tokio::test]
async fn health_that_is_not_json_still_means_down_not_a_panic() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.route(HEALTH, Answer::Raw("<html>"));
    assert_eq!(fetch_err(&base).await, FetchError::Invalid);
    // A health answer without a version is fine; the version is just missing.
    stub.ok(HEALTH, &json!({ "status": "ok" }));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok);
    assert!(!s.values.contains_key("version"));
}

// ---------------------------------------------------------------------------
// The key

#[tokio::test]
async fn a_refused_key_gives_no_reading() {
    for (code, expected) in [(401, FetchError::Auth), (403, FetchError::NotAdmin)] {
        let (base, stub) = stub::serve().await;
        quiet(&stub);
        stub.route(SESSIONS, Answer::Status(code));
        assert_eq!(fetch_err(&base).await, expected, "{code}");
    }
}

#[tokio::test]
async fn a_key_that_is_no_admin_key_gets_its_own_hint_from_any_admin_endpoint() {
    for path in [SESSIONS, LIBRARIES, TASKS] {
        let (base, stub) = stub::serve().await;
        quiet(&stub);
        stub.route(path, Answer::Status(403));
        let err = fetch_err(&base).await;
        assert_eq!(err, FetchError::NotAdmin, "{path}");
        assert_eq!(err.to_string(), "key is not an admin key");
    }
    // 401 on a slow endpoint is a refused key.
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.route(TASKS, Answer::Status(401));
    assert_eq!(fetch_err(&base).await, FetchError::Auth);
}

#[tokio::test]
async fn a_failing_fast_call_gives_a_warning_with_the_rest() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.route(SESSIONS, Answer::Status(500));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Warn);
    assert_eq!(s.error.as_deref(), Some("HTTP 500"));
    assert!(!s.values.contains_key("streams"));
    assert_eq!(num(&s, "tasks_running"), Some(1.0));
}

#[tokio::test]
async fn a_failing_call_does_not_hide_a_library_fault() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(LIBRARIES, &json!([library("A", 1, 0, 0, 0)]));
    stub.route(SESSIONS, Answer::Status(500));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(need_keys(&s), [LIBRARY_NEED]);
}

#[tokio::test]
async fn malformed_json_costs_only_that_call() {
    for path in [SESSIONS, LIBRARIES, TASKS] {
        for raw in ["", "garbage", "{}", "null", "42", "\"x\""] {
            let (base, stub) = stub::serve().await;
            quiet(&stub);
            stub.route(path, Answer::Raw(raw));
            let s = fetch(&base).await;
            assert_eq!(s.status, Status::Warn, "{path} {raw:?}");
            assert_eq!(
                s.error.as_deref(),
                Some("unexpected answer"),
                "{path} {raw:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_oversized_answer_is_refused_for_that_call_only() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    let big: Vec<Json> = (0..40_000)
        .map(|_| json!({ "name": "x".repeat(40), "unavailable_titles": 0 }))
        .collect();
    stub.ok(LIBRARIES, &json!(big));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Warn);
    assert_eq!(s.error.as_deref(), Some("response too large"));
    assert_eq!(num(&s, "streams"), Some(2.0));
}

#[tokio::test]
async fn no_key_is_a_settings_problem_and_nothing_is_sent() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    for key in [None, Some(""), Some("bad\nkey")] {
        let err = tofa().fetch(&ctx(&base, key)).await.unwrap_err();
        assert_eq!(err, FetchError::Config);
    }
    assert!(stub.seen().is_empty());
}

#[tokio::test]
async fn an_address_that_cannot_be_used_is_refused_before_any_request() {
    for bad in [
        "",
        "not a url",
        "ftp://example.com/",
        "http://user:pw@127.0.0.1:1/",
    ] {
        let err = tofa().fetch(&ctx(bad, Some(KEY))).await.unwrap_err();
        assert_eq!(err, FetchError::Config, "{bad:?}");
        assert!(!err.to_string().contains("pw"));
    }
    let err = tofa()
        .fetch(&ctx("http://169.254.169.254/", Some(KEY)))
        .await
        .unwrap_err();
    assert_eq!(err, FetchError::Forbidden);
}

#[tokio::test]
async fn names_and_titles_from_tofa_never_reach_a_need_or_the_error() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(
        LIBRARIES,
        &json!([library("Hemmelig\u{202E}Bibliotek", 4, 1, 1, 1)]),
    );
    stub.ok(
        SESSIONS,
        &sessions(&[("Transcode", "HlsFullTranscode", false)]),
    );
    let s = fetch(&base).await;
    // Names and titles are for the details only: never in a need, a number,
    // the error or the status.
    let outside = serde_json::to_string(&(&s.needs, &s.values, &s.error, s.status)).unwrap();
    for hidden in ["Hemmelig", "Film 0", "user0", "Tofa Web"] {
        assert!(!outside.contains(hidden), "{hidden} reached the reading");
    }
    // The user, the title and the client are in the playing list, as text.
    let blocks = details(&s);
    let items = list_of(&blocks, "tofa_playing").unwrap();
    assert_eq!(items[0].text, "Film 0");
    assert_eq!(items[0].sub.as_deref(), Some("user0 \u{b7} Tofa Web"));
    assert_eq!(items[0].badge_key.as_deref(), Some("tofa_transcode"));
    // The library name too, and nowhere else but the details.
    let libraries = list_of(&blocks, "tofa_libraries").unwrap();
    assert!(libraries[0].text.starts_with("Hemmelig"));
}

#[tokio::test]
async fn the_key_is_in_no_reading_error_or_log() {
    let _ = log::buffer();
    let mut snapshots = Vec::new();
    let mut errors = Vec::new();
    let answers: Vec<(Answer, Answer)> = vec![
        (Answer::Json(json!([])), Answer::Json(json!([]))),
        (Answer::Status(401), Answer::Status(401)),
        (Answer::Status(403), Answer::Status(403)),
        (Answer::Status(500), Answer::Status(404)),
        (Answer::Raw("garbage"), Answer::Raw("{")),
    ];
    for (sessions_answer, libraries_answer) in answers {
        let (base, stub) = stub::serve().await;
        quiet(&stub);
        stub.route(SESSIONS, sessions_answer);
        stub.route(LIBRARIES, libraries_answer);
        let c = ctx(&base, Some(KEY));
        match tofa().fetch(&c).await {
            Ok(s) => snapshots.push(s),
            Err(e) => errors.push(e),
        }
        let _ = format!("{c:?}");
    }
    // A server that is down, and an address nothing listens on.
    let (base, stub) = stub::serve().await;
    stub.route(HEALTH, Answer::Status(503));
    errors.push(fetch_err(&base).await);
    errors.push(fetch_err("http://127.0.0.1:1").await);

    let mut all = String::new();
    for s in &snapshots {
        all.push_str(&format!("{s:?}"));
        all.push_str(&serde_json::to_string(s).unwrap());
    }
    for e in &errors {
        all.push_str(&format!("{e} {e:?}"));
    }
    assert!(!errors.is_empty() && !snapshots.is_empty());
    assert!(!all.contains(KEY), "a reading holds the key");
    assert!(!all.contains("Bearer"), "a reading mentions the header");

    let log = log::logged();
    assert!(
        log::client_lines_seen(&log),
        "the capture sees the HTTP client's own lines"
    );
    assert!(!log.contains(KEY), "the log holds the key");
    assert!(!log.contains("Bearer"), "the log mentions the header");
}

#[tokio::test]
async fn a_redirect_to_another_origin_never_carries_the_key() {
    // The data endpoints are for admins and carry the key: a redirect to
    // another origin is refused, and the other origin never sees a key. The
    // public health call has none to lose and follows the redirect.
    for redirect in stub::redirects() {
        let (target, target_stub) = stub::serve().await;
        quiet(&target_stub);
        let origin = stub::redirector(redirect, format!("{target}{HEALTH}")).await;
        let s = fetch(&origin).await;
        // Health went across and answered; the first keyed call was refused
        // at the redirect.
        assert_eq!(s.status, Status::Warn);
        assert_eq!(s.error.as_deref(), Some("redirect not allowed"));
        let seen = target_stub.seen();
        assert!(!seen.is_empty());
        for hit in seen {
            assert_eq!(hit.authorization, None, "{}", hit.path);
            assert_eq!(hit.api_key, None, "{}", hit.path);
            assert_eq!(hit.path, HEALTH, "only the public call went across");
        }
    }
}

#[tokio::test]
async fn saving_or_testing_the_service_makes_the_slow_calls_run_again() {
    use mindful_magpie::integrations::api;
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    let slow = "/api/v1/libraries/health";
    // The first fetch refreshes the slow calls; the second carries them on.
    fetch(&base).await;
    fetch(&base).await;
    assert_eq!(stub.count(slow), 1);
    // Another address being saved changes nothing here.
    api::forget_all_for("http://127.0.0.1:1/");
    fetch(&base).await;
    assert_eq!(stub.count(slow), 1);
    // Saving or testing this one forgets what was remembered of it: the next
    // fetch counts as the first again.
    api::forget_all_for(&base);
    fetch(&base).await;
    assert_eq!(stub.count(slow), 2);
    fetch(&base).await;
    assert_eq!(stub.count(slow), 2);
}

// ---------------------------------------------------------------------------
// Details

use mindful_magpie::integrations::Block;
use mindful_magpie::integrations::block::{ListItem, MAX_ITEMS};

/// The reading's details as the poller stores them: cut down to the limits.
fn details(snapshot: &Snapshot) -> Vec<Block> {
    let mut bounded = snapshot.clone();
    bounded.bound_details();
    bounded.details
}

fn list_of<'a>(blocks: &'a [Block], title: &str) -> Option<&'a Vec<ListItem>> {
    blocks.iter().find_map(|b| match b {
        Block::List { title_key, items } if title_key == title => Some(items),
        _ => None,
    })
}

/// The bars of the progress block: label, percentage, text.
fn bars_of(blocks: &[Block]) -> Option<Vec<(String, f64, String)>> {
    blocks.iter().find_map(|b| match b {
        Block::Bars { title_key, bars } if title_key == "tofa_progress" => Some(
            bars.iter()
                .map(|bar| (bar.label.clone(), bar.pct, bar.text.clone()))
                .collect(),
        ),
        _ => None,
    })
}

fn facts_of(blocks: &[Block]) -> Vec<(String, String)> {
    blocks
        .iter()
        .find_map(|b| match b {
            Block::Facts { title_key, rows } if title_key == "tofa_server" => Some(
                rows.iter()
                    .map(|r| (r.label_key.clone().unwrap_or_default(), r.value.clone()))
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

/// A session as a real server lists it, with the fields the card shows (the
/// profile is the default label «Main»).
fn play(
    title: &str,
    user: &str,
    client: &str,
    mode: &str,
    paused: bool,
    progress: (i64, i64),
) -> Json {
    json!({
        "session_id": "00000000-0000-4000-8000-000000000001",
        "media_title": title,
        "username": user,
        "profile_name": "Main",
        "client_name": client,
        "is_paused": paused,
        "play_method": "DirectStream",
        "decision_mode": mode,
        "position_ms": progress.0,
        "duration_ms": progress.1,
    })
}

#[tokio::test]
async fn every_session_is_a_line_of_the_now_playing_list() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(
        SESSIONS,
        &json!([
            play(
                "Dune",
                "Kari",
                "Tofa Web",
                "DirectFile",
                false,
                (4_200_000, 5_000_000)
            ),
            play(
                "Alien",
                "Ola",
                "Living room TV",
                "HlsCopyVideoTranscodeAudio",
                true,
                (100, 0)
            ),
        ]),
    );
    let s = fetch(&base).await;
    let blocks = details(&s);
    let items = list_of(&blocks, "tofa_playing").expect("a now-playing list");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].text, "Dune");
    assert_eq!(items[0].sub.as_deref(), Some("Kari (Main) \u{b7} Tofa Web"));
    assert_eq!(items[0].right, None, "the progress is in the bars");
    assert_eq!(items[0].status, Some(Status::Ok));
    assert_eq!(items[0].href, None);
    assert_eq!(items[0].badge_key, None, "no transcode, no badge");
    // Paused: a warning. A transcode has a badge (a word the page shows in the
    // reader's language), not a word in the second line. No duration: no
    // progress.
    assert_eq!(items[1].text, "Alien");
    assert_eq!(
        items[1].sub.as_deref(),
        Some("Ola (Main) \u{b7} Living room TV")
    );
    assert_eq!(items[1].badge_key.as_deref(), Some("tofa_transcode"));
    assert_eq!(items[1].status, Some(Status::Warn));
    // Progress: one bar for the session that has one, with its title.
    let bars = bars_of(&blocks).expect("a progress block");
    assert_eq!(
        bars,
        [("Dune".to_string(), 84.0, "84 %".to_string())],
        "the one without a duration has no bar"
    );
    // The numbers and the needs are as before.
    assert_eq!(num(&s, "streams"), Some(1.0));
    assert_eq!(num(&s, "transcodes"), Some(1.0));
    assert_eq!(s.status, Status::Ok);
    assert!(s.needs.is_empty());
}

#[tokio::test]
async fn progress_is_a_whole_percentage_from_zero_to_a_hundred() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(
        SESSIONS,
        &json!([
            play("A", "p", "c", "DirectFile", false, (0, 1000)),
            play("B", "p", "c", "DirectFile", false, (5000, 1000)),
            play("C", "p", "c", "DirectFile", false, (1, 3)),
            play("D", "p", "c", "DirectFile", false, (1, -1)),
        ]),
    );
    let blocks = details(&fetch(&base).await);
    // The list keeps all four lines; the bars only the three with progress,
    // in the same order.
    assert_eq!(list_of(&blocks, "tofa_playing").unwrap().len(), 4);
    assert_eq!(
        bars_of(&blocks).unwrap(),
        [
            ("A".to_string(), 0.0, "0 %".to_string()),
            ("B".to_string(), 100.0, "100 %".to_string()),
            ("C".to_string(), 33.0, "33 %".to_string()),
        ]
    );
}

#[tokio::test]
async fn the_now_playing_list_holds_at_most_eight_and_hostile_titles_stay_text() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    let hostile = format!(
        "<img src=x onerror=alert(1)>\u{202E}\u{0}{}",
        "x".repeat(300)
    );
    let mut list: Vec<Json> = (0..20)
        .map(|i| play(&format!("Film {i}"), "p", "c", "DirectFile", false, (1, 2)))
        .collect();
    list[0] = play(&hostile, &hostile, &hostile, "DirectFile", false, (1, 2));
    stub.ok(SESSIONS, &json!(list));
    let s = fetch(&base).await;
    let blocks = details(&s);
    let items = list_of(&blocks, "tofa_playing").unwrap();
    assert_eq!(items.len(), MAX_ITEMS);
    assert!(items[0].text.starts_with("<img src=x"));
    let bars = bars_of(&blocks).unwrap();
    assert_eq!(bars.len(), MAX_ITEMS);
    assert!(bars[0].0.starts_with("<img src=x") && bars[0].0.chars().count() <= 80);
    for text in [Some(&items[0].text), items[0].sub.as_ref()] {
        let text = text.unwrap();
        assert!(text.chars().count() <= 80, "{text}");
        assert!(!text.contains(['\u{202E}', '\u{0}']), "{text}");
    }
    // Nothing from outside reaches the needs, the values or the error.
    let outside = serde_json::to_string(&(&s.needs, &s.values, &s.error)).unwrap();
    assert!(!outside.contains("onerror"), "{outside}");
    assert!(!outside.contains("Film"), "{outside}");
}

#[tokio::test]
async fn no_session_means_no_now_playing_list() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(SESSIONS, &json!([]));
    let blocks = details(&fetch(&base).await);
    assert!(list_of(&blocks, "tofa_playing").is_none());
}

#[tokio::test]
async fn the_libraries_list_shows_episodes_and_marks_issues() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    let with_episodes = |mut lib: Json, episodes: i64| {
        lib["episodes_total"] = json!(episodes);
        lib
    };
    stub.ok(
        LIBRARIES,
        &json!([
            with_episodes(library("Serier", 0, 0, 0, 0), 6576),
            library("Filmer", 0, 2, 0, 0),
            with_episodes(library("Gammelt", 4, 0, 0, 0), 12),
            library("Tomt", 0, 0, 0, 0),
        ]),
    );
    let s = fetch(&base).await;
    let blocks = details(&s);
    let items = list_of(&blocks, "tofa_libraries").expect("a libraries list");
    // The worst first; the others as listed.
    let rows: Vec<_> = items
        .iter()
        .map(|i| (i.text.as_str(), i.sub.as_deref(), i.status))
        .collect();
    assert_eq!(
        rows,
        [
            ("Gammelt", Some("12 ep."), Some(Status::Bad)),
            ("Filmer", None, Some(Status::Warn)),
            ("Serier", Some("6 576 ep."), Some(Status::Ok)),
            ("Tomt", None, Some(Status::Ok)),
        ]
    );
    // The needs still carry counts only.
    let outside = serde_json::to_string(&s.needs).unwrap();
    assert!(
        !outside.contains("Gammelt") && !outside.contains("Serier"),
        "{outside}"
    );
}

#[tokio::test]
async fn the_libraries_list_is_carried_between_the_slow_refreshes() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    fetch(&base).await;
    let s = fetch(&base).await;
    assert_eq!(stub.count(LIBRARIES), 1);
    let blocks = details(&s);
    assert!(list_of(&blocks, "tofa_libraries").is_some());
}

#[tokio::test]
async fn the_facts_hold_the_background_tasks_and_the_server_version() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(TASKS, &tasks(3));
    let blocks = details(&fetch(&base).await);
    assert_eq!(
        facts_of(&blocks),
        [
            ("tofa_tasks".to_string(), "3".to_string()),
            ("tofa_version".to_string(), "0.13.1".to_string()),
        ]
    );
    // A server without the tasks endpoint leaves that row out.
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.route(TASKS, Answer::Status(404));
    let blocks = details(&fetch(&base).await);
    assert_eq!(
        facts_of(&blocks),
        [("tofa_version".to_string(), "0.13.1".to_string())]
    );
}

#[tokio::test]
async fn every_key_the_tofa_details_use_has_a_text_in_both_languages() {
    let (base, stub) = stub::serve().await;
    quiet(&stub);
    stub.ok(
        SESSIONS,
        &json!([play("Dune", "Kari", "Web", "DirectFile", false, (1, 2))]),
    );
    let blocks = details(&fetch(&base).await);
    let mut keys: Vec<&str> = blocks.iter().flat_map(Block::keys).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(
        keys,
        [
            "tofa_libraries",
            "tofa_playing",
            "tofa_progress",
            "tofa_server",
            "tofa_tasks",
            "tofa_version"
        ]
    );
    for key in keys {
        locale::in_both(&format!("detail.{key}")).unwrap();
    }
}

#[test]
fn the_form_help_says_who_sees_what_is_playing() {
    let (en, nb) = (locale::LOCALES[0].1, locale::LOCALES[1].1);
    let help = |file| locale::text(file, "field", "tofa_secret_help").unwrap();
    assert!(
        help(en).ends_with("Everyone who can see the service sees what is playing."),
        "{}",
        help(en)
    );
    assert!(
        help(nb).ends_with("Alle som kan se tjenesten, ser hva som spilles."),
        "{}",
        help(nb)
    );
}
