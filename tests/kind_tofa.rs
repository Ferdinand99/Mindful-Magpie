//! The `tofa` kind («Tofa») against a stub of Tofa's API: the numbers, the
//! status and the needs, the capability gating, the staged refresh (fast
//! calls every fetch, slow calls every fourth), what a refused key, a failing
//! call or a malformed answer does, and that the key stays out of every
//! error, reading and log.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use mindful_magpie::integrations::tofa::{
    CAP_CHANGES, CAP_LIBRARIES, CAP_SESSIONS, CAP_TASKS, DOWN_NEED, LIBRARY_NEED, SLOW_EVERY,
};
use mindful_magpie::integrations::{
    self, FetchCtx, FetchError, Format, Level, Snapshot, Status, error_text_key,
};
use serde_json::{Value as Json, json};

const KEY: &str = "tofa_s3cr3t-K3y-9f8e7d6c";

// ---------------------------------------------------------------------------
// Capturing what is logged

#[derive(Clone)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn log_buffer() -> &'static LogBuffer {
    static BUFFER: OnceLock<LogBuffer> = OnceLock::new();
    static INSTALL: Once = Once::new();
    let buffer = BUFFER.get_or_init(|| LogBuffer(Arc::new(Mutex::new(Vec::new()))));
    INSTALL.call_once(|| {
        let writer = buffer.clone();
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .try_init()
            .expect("no other subscriber in this test binary");
    });
    buffer
}

fn logged() -> String {
    String::from_utf8_lossy(&log_buffer().0.lock().unwrap()).into_owned()
}

// ---------------------------------------------------------------------------
// A stub of Tofa

/// One request the stub saw.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    query: String,
    auth: Option<String>,
}

#[derive(Clone)]
struct Stub {
    /// What every address of this stub starts with: the kind remembers things
    /// per address, so each test has an address of its own.
    prefix: String,
    /// Path to status and body.
    replies: Arc<Mutex<HashMap<String, (u16, String)>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Stub {
    fn reply(&self, path: &str, status: u16, body: impl Into<String>) {
        self.replies
            .lock()
            .unwrap()
            .insert(path.to_string(), (status, body.into()));
    }

    fn ok(&self, path: &str, body: &Json) {
        self.reply(path, 200, body.to_string());
    }

    fn remove(&self, path: &str) {
        self.replies.lock().unwrap().remove(path);
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// How many requests went to `path`.
    fn hits(&self, path: &str) -> usize {
        self.seen().iter().filter(|s| s.path == path).count()
    }

    /// The queries of the requests to `path`.
    fn queries(&self, path: &str) -> Vec<String> {
        self.seen()
            .into_iter()
            .filter(|s| s.path == path)
            .map(|s| s.query)
            .collect()
    }

    fn forget_seen(&self) {
        self.seen.lock().unwrap().clear();
    }
}

async fn handle(State(stub): State<Stub>, request: Request) -> Response {
    let full = request.uri().path().to_string();
    let path = full.strip_prefix(&stub.prefix).unwrap_or(&full).to_string();
    stub.seen.lock().unwrap().push(Seen {
        path: path.clone(),
        query: request.uri().query().unwrap_or_default().to_string(),
        auth: request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
    });
    let reply = stub.replies.lock().unwrap().get(&path).cloned();
    match reply {
        Some((status, body)) => (
            StatusCode::from_u16(status).unwrap(),
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Starts the stub; the address without a trailing slash, unique per call.
async fn serve() -> (String, Stub) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let stub = Stub {
        prefix: format!("/tofa-{}", N.fetch_add(1, Ordering::Relaxed)),
        replies: Arc::default(),
        seen: Arc::default(),
    };
    let app = Router::new().fallback(handle).with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}{}", listener.local_addr().unwrap(), stub.prefix);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, stub)
}

const HEALTH: &str = "/api/v1/health";
const INFO: &str = "/api/v1/system/info";
const SESSIONS: &str = "/api/v1/system/playback/sessions";
const LIBRARIES: &str = "/api/v1/libraries/health";
const TASKS: &str = "/api/v1/system/tasks";
const CHANGES: &str = "/api/v1/media/changes";

// ---------------------------------------------------------------------------
// Fixtures in Tofa's shapes (from the OpenAPI document, v0.13.1)

fn health() -> Json {
    json!({
        "status": "ok",
        "database": "ok",
        "database_error": null,
        "http1_compat": true,
        "process_instance_id": "0b5f1b1e-5c36-4d77-9a0d-2f3b0f0e9d11",
        "server_id": "0f6c5d5a-7d0e-4a56-8d58-6a1f8f0c2b11",
        "version": "0.13.1",
    })
}

fn info_with(capabilities: &[&str]) -> Json {
    json!({
        "version": "0.13.1",
        "api_version": 7,
        "capabilities": capabilities,
        "connection_type": "direct",
        "discover_enabled": false,
        "library_count": 3,
        "user_count": 2,
        "port": 33333,
        "relay_active": false,
        "relay_endpoint": null,
        "relay_latency_ms": null,
        "requests_available": true,
        "server_id": "0f6c5d5a-7d0e-4a56-8d58-6a1f8f0c2b11",
    })
}

fn all_capabilities() -> Json {
    info_with(&[
        "sync.user_state",
        "ws.wt_contract_v1",
        CAP_SESSIONS,
        CAP_LIBRARIES,
        CAP_TASKS,
        CAP_CHANGES,
    ])
}

/// One session per play method; `Transcode` ones have a reason.
fn sessions(methods: &[&str]) -> Json {
    json!(
        methods
            .iter()
            .enumerate()
            .map(|(i, method)| json!({
                "session_id": format!("00000000-0000-4000-8000-00000000000{i}"),
                "user_id": "11111111-1111-4111-8111-111111111111",
                "username": format!("user{i}"),
                "profile_id": null,
                "profile_name": "Main",
                "media_file_id": "22222222-2222-4222-8222-222222222222",
                "media_id": "33333333-3333-4333-8333-333333333333",
                "media_title": format!("Film {i}"),
                "client_name": "Tofa Web",
                "device_name": "Firefox",
                "device_model": null,
                "created_at": "2026-11-07T19:00:00Z",
                "last_access": "2026-11-07T19:05:00Z",
                "position_ms": 120_000,
                "duration_ms": 7_200_000,
                "is_paused": i == 1,
                "max_bitrate_kbps": null,
                "play_method": method,
                "decision_mode": if *method == "Transcode" { "HlsFullTranscode" } else { "DirectFile" },
                "decision_trace_id": null,
                "selected_audio_stream_index": null,
                "stream_dir": null,
                "transcode_reason_codes": if *method == "Transcode" { json!(["VideoCodecNotSupported"]) } else { json!([]) },
            }))
            .collect::<Vec<_>>()
    )
}

/// A library of the shape of `LibraryHealth`: everything fine unless told.
fn library(name: &str, unavailable: i64, probe_failed: i64, overlaps: usize) -> Json {
    json!({
        "library_id": "44444444-4444-4444-8444-444444444444",
        "name": name,
        "media_type": "movie",
        "library_kind": "movie",
        "metadata_enabled": true,
        "unmatched_titles": 12,
        "match_pending_titles": 0,
        "unavailable_titles": unavailable,
        "skipped_files": 2,
        "probe_failed_files": probe_failed,
        "unbound_episode_files": 0,
        "episodes_total": 0,
        "episodes_flagged": 0,
        "episodes_verified": 0,
        "episodes_titled": 0,
        "episodes_complete": 0,
        "accepted_titles": 0,
        "folder_overlaps": (0..overlaps).map(|i| json!({
            "folder": format!("/media/a{i}"),
            "other_folder": format!("/media/b{i}"),
            "other_library_id": "55555555-5555-4555-8555-555555555555",
            "other_library_name": "Other",
        })).collect::<Vec<_>>(),
    })
}

fn tasks(statuses: &[&str]) -> Json {
    json!(
        statuses
            .iter()
            .map(|status| json!({
                "id": "66666666-6666-4666-8666-666666666666",
                "media_file_id": "77777777-7777-4777-8777-777777777777",
                "status": status,
                "progress": 0.4,
                "started_at": "2026-11-07T19:00:00Z",
                "created_at": "2026-11-07T18:59:00Z",
            }))
            .collect::<Vec<_>>()
    )
}

/// One change: media id, revision and kind.
type Change = (&'static str, i64, &'static str);

fn changes(latest: i64, full_sync: bool, list: &[Change]) -> Json {
    json!({
        "changes": list.iter().map(|(id, revision, kind)| json!({
            "media_id": id,
            "library_id": null,
            "revision": revision,
            "change_kind": kind,
        })).collect::<Vec<_>>(),
        "latest_revision": latest,
        "requires_full_sync": full_sync,
    })
}

/// The answers of a quiet Tofa that advertises every capability: two streams
/// (one a transcode), nothing wrong, one task running, nothing new.
fn quiet(stub: &Stub) {
    stub.ok(HEALTH, &health());
    stub.ok(INFO, &all_capabilities());
    stub.ok(SESSIONS, &sessions(&["DirectPlay", "Transcode"]));
    stub.ok(LIBRARIES, &json!([library("Films", 0, 0, 0)]));
    stub.ok(TASKS, &tasks(&["running"]));
    stub.ok(CHANGES, &changes(100, false, &[]));
}

fn ctx(base: &str, secret: Option<&str>) -> FetchCtx {
    FetchCtx {
        http: integrations::build_client(Duration::from_secs(5)),
        settings: json!({ "url": base }),
        secret: secret.map(str::to_string),
    }
}

/// A fetch with the key that is expected to give a reading, good or bad.
async fn fetch(base: &str) -> Snapshot {
    integrations::find("tofa")
        .expect("the registry lists tofa")
        .fetch(&ctx(base, Some(KEY)))
        .await
        .expect("a reading, good or bad")
}

/// A fetch with the key that is expected to give none: the reason.
async fn fetch_err(base: &str) -> FetchError {
    integrations::find("tofa")
        .expect("the registry lists tofa")
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

// ---------------------------------------------------------------------------
// The kind

#[test]
fn the_kind_describes_itself() {
    let kind = integrations::find("tofa").expect("the registry lists tofa");
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
            ("new_items", Format::Number),
            ("scans_running", Format::Number),
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

/// The keys of a locale file's section, with their texts.
fn locale(lang: &str, section: &str) -> HashMap<String, String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("locales")
        .join(format!("{lang}.toml"));
    let source = std::fs::read_to_string(path).unwrap();
    let mut current = "";
    let mut out = HashMap::new();
    for line in source.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name;
        } else if current == section
            && let Some((key, text)) = line.split_once(" = ")
        {
            out.insert(key.to_string(), text.trim_matches('"').to_string());
        }
    }
    out
}

#[test]
fn every_text_of_the_kind_exists_in_both_languages() {
    let kind = integrations::find("tofa").unwrap();
    let mut keys = vec![kind.label_key().to_string()];
    for field in kind.fields() {
        keys.push(field.label_key.to_string());
        keys.push(field.help_key.to_string());
    }
    keys.extend(kind.metrics().iter().map(|m| m.label_key.to_string()));
    keys.extend(["need.tofa_down", "need.tofa_library"].map(str::to_string));
    for lang in ["en", "nb"] {
        for key in &keys {
            let (section, name) = key.split_once('.').unwrap();
            let text = locale(lang, section)
                .remove(name)
                .unwrap_or_else(|| panic!("{lang}: no text for {key}"));
            assert!(!text.is_empty(), "{lang}: {key} is empty");
            // The chip already shows the service's name.
            assert!(!text.contains("{name}"), "{lang}: {key}: {text}");
        }
    }
}

#[test]
fn the_texts_say_what_was_agreed() {
    let need = |lang: &str, key: &str| locale(lang, "need").remove(key).unwrap();
    assert_eq!(need("en", "tofa_down"), "Not answering");
    assert_eq!(need("nb", "tofa_down"), "Svarer ikke");
    assert_eq!(need("en", "tofa_library"), "Libraries with errors: {count}");
    assert_eq!(need("nb", "tofa_library"), "Bibliotek med feil: {count}");

    // The form help: where the key is made, the server version, that the key
    // is an admin's, stored encrypted, and works on a direct connection only.
    let help = |lang: &str| locale(lang, "field").remove("tofa_secret_help").unwrap();
    let en = help("en");
    for part in [
        "Server → Settings → API keys",
        "0.9.34",
        "admin",
        "encrypted",
        "never shown again",
        "direct connection",
        "Authorization: Bearer",
    ] {
        assert!(en.contains(part), "en help lacks {part:?}: {en}");
    }
    let nb = help("nb");
    for part in [
        "Server → Settings → API keys",
        "0.9.34",
        "admin",
        "kryptert",
        "aldri igjen",
        "direkte tilkobling",
        "Authorization: Bearer",
    ] {
        assert!(nb.contains(part), "nb help lacks {part:?}: {nb}");
    }
    let label = |lang: &str| locale(lang, "field").remove("tofa_secret").unwrap();
    assert_eq!(label("en"), "API key");
    assert_eq!(label("nb"), "API-nøkkel");
}

#[test]
fn the_refused_key_text_maps_to_its_translation_key() {
    assert_eq!(FetchError::Auth.text_key(), Some("fetch_err.auth"));
    assert_eq!(error_text_key("key refused"), Some("fetch_err.auth"));
}

// ---------------------------------------------------------------------------
// A full set of answers

#[tokio::test]
async fn a_quiet_tofa_gives_every_number_and_is_ok() {
    let (base, stub) = serve().await;
    quiet(&stub);
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok, "{:?}", s.error);
    assert_eq!(s.error, None);
    assert_eq!(
        num(&s, "streams"),
        Some(2.0),
        "a paused one is a stream too"
    );
    assert_eq!(num(&s, "transcodes"), Some(1.0));
    assert_eq!(num(&s, "new_items"), Some(0.0));
    assert_eq!(num(&s, "scans_running"), Some(1.0));
    assert_eq!(num(&s, "libraries_unhealthy"), Some(0.0));
    assert_eq!(text(&s, "version").as_deref(), Some("0.13.1"));
    assert_eq!(s.values.len(), 6);
    assert!(s.needs.is_empty());
}

#[tokio::test]
async fn the_first_fetch_makes_every_call_once_with_the_key() {
    let (base, stub) = serve().await;
    quiet(&stub);
    fetch(&base).await;
    let seen = stub.seen();
    for path in [HEALTH, INFO, SESSIONS, LIBRARIES, TASKS, CHANGES] {
        assert_eq!(stub.hits(path), 1, "{path}: {seen:?}");
    }
    assert_eq!(seen.len(), 6);
    let want = format!("Bearer {KEY}");
    assert!(
        seen.iter()
            .all(|s| s.auth.as_deref() == Some(want.as_str())),
        "{seen:?}"
    );
}

#[tokio::test]
async fn the_address_may_carry_a_path_prefix() {
    // The stub's addresses all do: every call went under the prefix.
    let (base, stub) = serve().await;
    quiet(&stub);
    assert!(base.matches('/').count() >= 3);
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok);
    assert_eq!(stub.seen().len(), 6);
}

#[tokio::test]
async fn transcodes_are_the_sessions_that_transcode() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(
        SESSIONS,
        &sessions(&["Transcode", "DirectStream", "Transcode", "DirectPlay"]),
    );
    let s = fetch(&base).await;
    assert_eq!(num(&s, "streams"), Some(4.0));
    assert_eq!(num(&s, "transcodes"), Some(2.0));
    let (base, stub2) = serve().await;
    quiet(&stub2);
    stub2.ok(SESSIONS, &sessions(&[]));
    let s = fetch(&base).await;
    assert_eq!(num(&s, "streams"), Some(0.0));
    assert_eq!(num(&s, "transcodes"), Some(0.0));
    assert_eq!(s.status, Status::Ok);
}

// ---------------------------------------------------------------------------
// Staging

#[tokio::test]
async fn between_the_slow_refreshes_only_the_fast_calls_run_and_slow_values_are_carried() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(
        LIBRARIES,
        &json!([library("Films", 0, 0, 0), library("TV", 1, 0, 0)]),
    );
    stub.ok(TASKS, &tasks(&["running", "running"]));
    let first = fetch(&base).await;
    assert_eq!(num(&first, "libraries_unhealthy"), Some(1.0));
    assert_eq!(num(&first, "scans_running"), Some(2.0));
    stub.forget_seen();

    // Everything slow now answers differently; the next three fetches must
    // not ask, and must show the old values.
    stub.ok(LIBRARIES, &json!([library("Films", 0, 0, 0)]));
    stub.ok(TASKS, &tasks(&[]));
    stub.ok(SESSIONS, &sessions(&["DirectPlay"]));
    for round in 1..SLOW_EVERY {
        let s = fetch(&base).await;
        assert_eq!(
            num(&s, "streams"),
            Some(1.0),
            "round {round}: fast value is fresh"
        );
        assert_eq!(
            num(&s, "libraries_unhealthy"),
            Some(1.0),
            "round {round}: slow value is carried"
        );
        assert_eq!(num(&s, "scans_running"), Some(2.0), "round {round}");
        assert_eq!(s.status, Status::Bad, "round {round}");
        assert_eq!(s.needs.len(), 1, "round {round}");
    }
    assert_eq!(stub.hits(HEALTH), 3);
    assert_eq!(stub.hits(SESSIONS), 3);
    for path in [LIBRARIES, TASKS, CHANGES] {
        assert_eq!(
            stub.hits(path),
            0,
            "{path} must not be asked between refreshes"
        );
    }
    assert_eq!(stub.hits(INFO), 0, "the info is remembered");

    // The fourth fetch after the first refreshes them.
    let s = fetch(&base).await;
    assert_eq!(stub.hits(LIBRARIES), 1);
    assert_eq!(stub.hits(TASKS), 1);
    assert_eq!(stub.hits(CHANGES), 1);
    assert_eq!(num(&s, "libraries_unhealthy"), Some(0.0));
    assert_eq!(num(&s, "scans_running"), Some(0.0));
    assert_eq!(s.status, Status::Ok);
    assert!(s.needs.is_empty());
}

#[tokio::test]
async fn a_failing_slow_call_keeps_the_carried_value_and_says_so() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(LIBRARIES, &json!([library("TV", 1, 0, 0)]));
    fetch(&base).await;
    for _ in 1..SLOW_EVERY {
        fetch(&base).await;
    }
    stub.reply(LIBRARIES, 500, "{}");
    let s = fetch(&base).await;
    assert_eq!(
        num(&s, "libraries_unhealthy"),
        Some(1.0),
        "the old value stays"
    );
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("HTTP 500"));
    assert_eq!(
        num(&s, "scans_running"),
        Some(1.0),
        "the other calls still read"
    );
}

#[tokio::test]
async fn the_info_is_asked_once_and_not_again_while_it_is_remembered() {
    let (base, stub) = serve().await;
    quiet(&stub);
    for _ in 0..(SLOW_EVERY * 2 + 1) {
        fetch(&base).await;
    }
    assert_eq!(stub.hits(INFO), 1);
    assert_eq!(stub.hits(SESSIONS), (SLOW_EVERY * 2 + 1) as usize);
    assert_eq!(stub.hits(LIBRARIES), 3);
}

#[tokio::test]
async fn services_at_other_addresses_have_their_own_rhythm_and_memory() {
    let (a, stub_a) = serve().await;
    let (b, stub_b) = serve().await;
    quiet(&stub_a);
    quiet(&stub_b);
    fetch(&a).await;
    fetch(&a).await;
    fetch(&b).await;
    assert_eq!(stub_a.hits(LIBRARIES), 1);
    assert_eq!(
        stub_b.hits(LIBRARIES),
        1,
        "b's first fetch refreshes the slow calls"
    );
    assert_eq!(stub_b.hits(INFO), 1);
}

// ---------------------------------------------------------------------------
// Capabilities

#[tokio::test]
async fn an_endpoint_the_capabilities_do_not_allow_is_not_called() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(
        INFO,
        &info_with(&[CAP_SESSIONS, CAP_LIBRARIES, "sync.user_state"]),
    );
    let s = fetch(&base).await;
    assert_eq!(stub.hits(SESSIONS), 1);
    assert_eq!(stub.hits(LIBRARIES), 1);
    assert_eq!(stub.hits(TASKS), 0);
    assert_eq!(stub.hits(CHANGES), 0);
    assert_eq!(num(&s, "streams"), Some(2.0));
    assert_eq!(num(&s, "libraries_unhealthy"), Some(0.0));
    assert_eq!(num(&s, "scans_running"), None);
    assert_eq!(num(&s, "new_items"), None);
    assert_eq!(s.status, Status::Ok, "a missing capability is not a fault");
    assert_eq!(s.error, None);

    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(INFO, &info_with(&[CAP_TASKS, CAP_CHANGES]));
    let s = fetch(&base).await;
    assert_eq!(stub.hits(SESSIONS), 0);
    assert_eq!(stub.hits(LIBRARIES), 0);
    assert_eq!(num(&s, "streams"), None);
    assert_eq!(num(&s, "transcodes"), None);
    assert_eq!(num(&s, "libraries_unhealthy"), None);
    assert_eq!(num(&s, "scans_running"), Some(1.0));
    assert_eq!(num(&s, "new_items"), Some(0.0));
    // Health and info are never gated.
    assert_eq!(stub.hits(HEALTH), 1);
    assert_eq!(stub.hits(INFO), 1);
}

#[tokio::test]
async fn no_capability_list_gates_nothing() {
    for info in [
        info_with(&[]),
        json!({ "version": "0.9.34" }),
        json!({ "version": "0.9.34", "capabilities": null }),
    ] {
        let (base, stub) = serve().await;
        quiet(&stub);
        stub.ok(INFO, &info);
        let s = fetch(&base).await;
        for path in [SESSIONS, LIBRARIES, TASKS, CHANGES] {
            assert_eq!(stub.hits(path), 1, "{path} with {info}");
        }
        assert_eq!(s.values.len(), 6, "{info}");
    }
}

#[tokio::test]
async fn an_info_that_does_not_exist_or_fails_gates_nothing() {
    for (status, body) in [(404, ""), (500, "{}"), (200, "not json")] {
        let (base, stub) = serve().await;
        quiet(&stub);
        stub.reply(INFO, status, body);
        let s = fetch(&base).await;
        for path in [SESSIONS, LIBRARIES, TASKS, CHANGES] {
            assert_eq!(stub.hits(path), 1, "{path} with info {status}");
        }
        assert_eq!(s.status, Status::Ok, "info {status}: {:?}", s.error);
        assert_eq!(s.error, None, "info {status}");
        // The version still shows, from the health answer.
        assert_eq!(
            text(&s, "version").as_deref(),
            Some("0.13.1"),
            "info {status}"
        );
    }
}

#[tokio::test]
async fn an_endpoint_that_answers_404_is_left_alone_for_a_while() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(INFO, &info_with(&[]));
    stub.remove(SESSIONS);
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok);
    assert_eq!(s.error, None);
    assert_eq!(num(&s, "streams"), None);
    fetch(&base).await;
    assert_eq!(stub.hits(SESSIONS), 1, "a 404 is not asked again");
    assert_eq!(stub.hits(HEALTH), 2, "health is always asked");
}

// ---------------------------------------------------------------------------
// Status and needs

#[tokio::test]
async fn a_server_that_does_not_answer_is_bad_with_the_down_need() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.reply(HEALTH, 503, health().to_string());
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("HTTP 503"));
    assert_eq!(s.needs.len(), 1);
    let need = &s.needs[0];
    assert_eq!(need.key, DOWN_NEED);
    assert_eq!(need.key, "tofa:down");
    assert_eq!(need.level, Level::Bad);
    assert_eq!(need.label_key, "need.tofa_down");
    assert!(need.params.is_empty());
    assert!(s.values.is_empty());
    // Nothing else is asked of a server that says it is not well.
    assert_eq!(stub.seen().len(), 1);
}

#[tokio::test]
async fn an_address_that_cannot_be_reached_or_is_not_tofa_is_down_too() {
    let s = fetch("http://127.0.0.1:1").await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("connection failed"));
    assert_eq!(s.needs[0].key, DOWN_NEED);

    let (base, _stub) = serve().await; // answers 404 to everything
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("HTTP 404"));
    assert_eq!(s.needs[0].key, DOWN_NEED);

    let (base, stub) = serve().await;
    stub.reply(HEALTH, 200, "<html>login</html>");
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("unexpected answer"));
    assert_eq!(s.needs[0].key, DOWN_NEED);
}

#[tokio::test]
async fn a_server_that_comes_back_is_fine_again() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.reply(HEALTH, 503, "{}");
    assert_eq!(fetch(&base).await.status, Status::Bad);
    stub.ok(HEALTH, &health());
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok, "{:?}", s.error);
    assert!(s.needs.is_empty());
}

#[tokio::test]
async fn an_unhealthy_library_is_bad_with_a_counted_need() {
    // Titles that are unavailable, files that fail to probe, overlapping
    // folders: three libraries; one fine; one with only unmatched titles.
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(
        LIBRARIES,
        &json!([
            library("Fine", 0, 0, 0),
            library("Gone", 3, 0, 0),
            library("Broken", 0, 2, 0),
            library("Overlap", 0, 0, 1),
        ]),
    );
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(num(&s, "libraries_unhealthy"), Some(3.0));
    assert_eq!(s.needs.len(), 1);
    let need = &s.needs[0];
    assert_eq!(need.key, LIBRARY_NEED);
    assert_eq!(need.key, "tofa:library");
    assert_eq!(need.level, Level::Bad);
    assert_eq!(need.label_key, "need.tofa_library");
    assert_eq!(need.score, 3.0);
    assert_eq!(need.params.len(), 1);
    assert_eq!(need.params["count"], "3");
}

#[tokio::test]
async fn unmatched_titles_alone_are_not_a_fault() {
    let (base, stub) = serve().await;
    quiet(&stub);
    let mut lib = library("Films", 0, 0, 0);
    lib["unmatched_titles"] = json!(900);
    lib["skipped_files"] = json!(40);
    stub.ok(LIBRARIES, &json!([lib]));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok);
    assert!(s.needs.is_empty());
    assert_eq!(num(&s, "libraries_unhealthy"), Some(0.0));
}

#[tokio::test]
async fn a_failed_task_warns() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(TASKS, &tasks(&["running", "failed"]));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Warn);
    assert_eq!(
        num(&s, "scans_running"),
        Some(1.0),
        "a failed task is not running"
    );
    assert!(s.needs.is_empty(), "a warning, not a need");
}

#[tokio::test]
async fn a_bad_library_beats_a_failed_task() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(TASKS, &tasks(&["failed"]));
    stub.ok(LIBRARIES, &json!([library("Gone", 1, 0, 0)]));
    assert_eq!(fetch(&base).await.status, Status::Bad);
}

#[tokio::test]
async fn names_and_titles_from_tofa_never_reach_a_need_or_the_error() {
    let hostile = "Evil\u{202E}Lib \"quoted\" <script>alert(1)</script>";
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(LIBRARIES, &json!([library(hostile, 4, 0, 0)]));
    stub.ok(SESSIONS, &{
        let mut list = sessions(&["Transcode"]);
        list[0]["media_title"] = json!(hostile);
        list[0]["username"] = json!(hostile);
        list
    });
    let mut info = all_capabilities();
    info["version"] = json!(format!("1.0\u{202E}-{}", "x".repeat(300)));
    stub.ok(INFO, &info);
    let s = fetch(&base).await;
    assert_eq!(s.needs.len(), 1);
    let needs = serde_json::to_string(&s.needs).unwrap();
    for part in ["Evil", "script", "quoted", "Film"] {
        assert!(!needs.contains(part), "{needs}");
    }
    let stored = serde_json::to_string(&s).unwrap();
    assert!(
        !stored.contains("Film") && !stored.contains("user0"),
        "{stored}"
    );
    assert!(
        !stored.contains("Lib"),
        "library names are not kept: {stored}"
    );
    // The version is external text: cleaned and cut.
    let version = text(&s, "version").unwrap();
    assert!(!version.contains('\u{202E}'), "{version}");
    assert!(version.chars().count() <= 40, "{version}");
}

// ---------------------------------------------------------------------------
// New in library

#[tokio::test]
async fn new_items_count_from_a_baseline_taken_on_the_first_fetch() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(CHANGES, &changes(100, false, &[("x", 100, "upsert")]));
    let first = fetch(&base).await;
    assert_eq!(
        num(&first, "new_items"),
        Some(0.0),
        "the baseline itself is not new"
    );
    assert_eq!(stub.queries(CHANGES), ["limit=1"]);

    for _ in 1..SLOW_EVERY {
        fetch(&base).await;
    }
    // Two items added, one of them refreshed twice, one deleted again, one
    // upsert whose last word is a delete, and a delete of something old.
    stub.ok(
        CHANGES,
        &changes(
            110,
            false,
            &[
                ("a", 101, "upsert"),
                ("a", 105, "upsert"),
                ("b", 102, "upsert"),
                ("c", 103, "upsert"),
                ("c", 104, "delete"),
                ("old", 106, "delete"),
            ],
        ),
    );
    let s = fetch(&base).await;
    assert_eq!(num(&s, "new_items"), Some(2.0));
    assert_eq!(
        stub.queries(CHANGES).last().map(String::as_str),
        Some("since_revision=100&limit=500"),
        "the baseline stays where the day began"
    );

    // Carried between the refreshes.
    for _ in 1..SLOW_EVERY {
        let s = fetch(&base).await;
        assert_eq!(num(&s, "new_items"), Some(2.0));
    }
    assert_eq!(stub.hits(CHANGES), 2);
}

#[tokio::test]
async fn a_full_sync_answer_starts_the_count_again() {
    let (base, stub) = serve().await;
    quiet(&stub);
    fetch(&base).await;
    for _ in 1..SLOW_EVERY {
        fetch(&base).await;
    }
    stub.ok(CHANGES, &changes(5000, true, &[("a", 4999, "upsert")]));
    let s = fetch(&base).await;
    assert_eq!(num(&s, "new_items"), Some(0.0));
    for _ in 1..SLOW_EVERY {
        fetch(&base).await;
    }
    fetch(&base).await;
    assert_eq!(
        stub.queries(CHANGES).last().map(String::as_str),
        Some("since_revision=5000&limit=500")
    );
}

#[tokio::test]
async fn an_answer_without_a_latest_revision_costs_only_that_number() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(CHANGES, &json!({ "changes": [] }));
    let s = fetch(&base).await;
    assert_eq!(num(&s, "new_items"), None);
    assert_eq!(s.error.as_deref(), Some("unexpected answer"));
    assert_eq!(s.status, Status::Warn);
    assert_eq!(num(&s, "streams"), Some(2.0));
    assert_eq!(num(&s, "libraries_unhealthy"), Some(0.0));
}

// ---------------------------------------------------------------------------
// Failures

#[tokio::test]
async fn a_failing_fast_call_gives_a_warning_with_the_rest() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.reply(SESSIONS, 500, "{}");
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Warn);
    assert_eq!(s.error.as_deref(), Some("HTTP 500"));
    assert_eq!(num(&s, "streams"), None);
    assert_eq!(num(&s, "scans_running"), Some(1.0));
    assert!(s.needs.is_empty());
}

#[tokio::test]
async fn a_failing_call_does_not_hide_a_library_fault() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.reply(SESSIONS, 500, "{}");
    stub.ok(LIBRARIES, &json!([library("Gone", 1, 0, 0)]));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("HTTP 500"));
    assert_eq!(s.needs.len(), 1);
}

#[tokio::test]
async fn a_refused_key_gives_no_reading() {
    for (path, status) in [
        (INFO, 401),
        (SESSIONS, 401),
        (SESSIONS, 403),
        (LIBRARIES, 403),
        (TASKS, 401),
        (CHANGES, 403),
    ] {
        let (base, stub) = serve().await;
        quiet(&stub);
        stub.reply(path, status, format!("Bearer {KEY}"));
        let err = fetch_err(&base).await;
        assert_eq!(err, FetchError::Auth, "{path} {status}");
        assert_eq!(err.to_string(), "key refused");
    }
}

#[tokio::test]
async fn malformed_json_is_a_warning_or_down_not_a_panic() {
    let (base, stub) = serve().await;
    quiet(&stub);
    for path in [SESSIONS, LIBRARIES, TASKS, CHANGES] {
        stub.reply(path, 200, "{ not json");
    }
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Warn);
    assert_eq!(s.error.as_deref(), Some("unexpected answer"));
    assert!(num(&s, "streams").is_none() && num(&s, "scans_running").is_none());
    assert_eq!(text(&s, "version").as_deref(), Some("0.13.1"));

    // The right JSON in the wrong shape.
    let (base, stub) = serve().await;
    quiet(&stub);
    for path in [SESSIONS, LIBRARIES, TASKS] {
        stub.ok(path, &json!({ "sessions": [] }));
    }
    let s = fetch(&base).await;
    assert_eq!(s.error.as_deref(), Some("unexpected answer"));
    assert_eq!(s.status, Status::Warn);
    assert_eq!(num(&s, "streams"), None);
    assert_eq!(num(&s, "libraries_unhealthy"), None);
    assert_eq!(num(&s, "scans_running"), None);
    assert_eq!(num(&s, "new_items"), Some(0.0));

    // Odd values inside the lists cost nothing.
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(
        SESSIONS,
        &json!([null, 5, { "play_method": 7 }, { "play_method": "Transcode" }]),
    );
    stub.ok(
        LIBRARIES,
        &json!([null, "x", { "unavailable_titles": "many" }, { "probe_failed_files": -3 }]),
    );
    stub.ok(TASKS, &json!([null, 3, { "status": 4 }]));
    let s = fetch(&base).await;
    assert_eq!(s.error, None);
    assert_eq!(num(&s, "streams"), Some(4.0));
    assert_eq!(num(&s, "transcodes"), Some(1.0));
    assert_eq!(num(&s, "libraries_unhealthy"), Some(0.0));
    assert_eq!(num(&s, "scans_running"), Some(3.0));
}

#[tokio::test]
async fn an_oversized_answer_is_refused_for_that_call_only() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.reply(
        SESSIONS,
        200,
        format!("[\"{}\"]", "x".repeat(2 * 1024 * 1024)),
    );
    let s = fetch(&base).await;
    assert_eq!(s.error.as_deref(), Some("response too large"));
    assert_eq!(s.status, Status::Warn);
    assert_eq!(num(&s, "streams"), None);
    assert_eq!(num(&s, "scans_running"), Some(1.0));
}

#[tokio::test]
async fn no_key_is_a_settings_problem_and_nothing_is_sent() {
    let (base, stub) = serve().await;
    quiet(&stub);
    for secret in [None, Some(""), Some("a\r\nb")] {
        let err = integrations::find("tofa")
            .unwrap()
            .fetch(&ctx(&base, secret))
            .await
            .unwrap_err();
        assert_eq!(err, FetchError::Config, "{secret:?}");
    }
    assert!(stub.seen().is_empty());
}

#[tokio::test]
async fn an_address_that_cannot_be_used_is_refused_before_any_request() {
    let kind = integrations::find("tofa").unwrap();
    let at = |settings: Json| FetchCtx {
        http: integrations::build_client(Duration::from_secs(5)),
        settings,
        secret: Some(KEY.to_string()),
    };
    for bad in ["", "nope", "ftp://h/", "http://u:p@h/"] {
        let err = kind.fetch(&at(json!({ "url": bad }))).await.unwrap_err();
        assert_eq!(err, FetchError::Config, "{bad:?}");
    }
    assert_eq!(
        kind.fetch(&at(json!({}))).await.unwrap_err(),
        FetchError::Config
    );
    for bad in ["http://169.254.169.254/", "http://[fe80::1]/"] {
        let err = kind.fetch(&at(json!({ "url": bad }))).await.unwrap_err();
        assert_eq!(err, FetchError::Forbidden, "{bad}");
    }
}

// ---------------------------------------------------------------------------
// The key stays secret

#[tokio::test]
async fn the_key_is_in_no_reading_error_or_log() {
    let _ = log_buffer();
    let mut readings = Vec::new();
    let mut errors = Vec::new();

    // A good set, a failing call, a down server.
    let (base, stub) = serve().await;
    quiet(&stub);
    readings.push(fetch(&base).await);
    stub.reply(SESSIONS, 500, format!("{{\"echo\": \"Bearer {KEY}\"}}"));
    readings.push(fetch(&base).await);
    stub.reply(HEALTH, 503, format!("{{\"echo\": \"Bearer {KEY}\"}}"));
    readings.push(fetch(&base).await);
    // A refusal, malformed answers, an unreachable server.
    let (other, other_stub) = serve().await;
    quiet(&other_stub);
    other_stub.reply(INFO, 401, format!("Bearer {KEY}"));
    errors.push(fetch_err(&other).await);
    let (third, third_stub) = serve().await;
    quiet(&third_stub);
    for path in [SESSIONS, LIBRARIES, TASKS, CHANGES] {
        third_stub.reply(path, 200, format!("{{\"x\": \"{KEY}\"}}"));
    }
    readings.push(fetch(&third).await);
    readings.push(fetch("http://127.0.0.1:1").await);
    for forbidden in ["http://169.254.169.254/", "ftp://x/"] {
        errors.push(
            integrations::find("tofa")
                .unwrap()
                .fetch(&ctx(forbidden, Some(KEY)))
                .await
                .unwrap_err(),
        );
    }
    for err in &errors {
        assert!(!format!("{err} {err:?}").contains(KEY));
    }

    for reading in &readings {
        let stored = serde_json::to_string(reading).unwrap();
        assert!(!stored.contains(KEY), "{stored}");
        assert!(!stored.contains("Bearer"), "{stored}");
        assert!(!format!("{reading:?}").contains(KEY));
    }

    let c = ctx(&base, Some(KEY));
    assert!(!format!("{c:?}").contains(KEY));

    let log = logged();
    // The capture does see the lines of the libraries underneath (hyper and
    // reqwest log at debug and trace).
    assert!(
        log.contains("hyper_util") || log.contains("reqwest"),
        "the capture sees the HTTP client's own lines"
    );
    assert!(!log.contains(KEY), "the key reached the log");
    assert!(
        !log.contains("Bearer"),
        "an Authorization value reached the log"
    );
}

#[tokio::test]
async fn a_redirect_to_another_origin_does_not_carry_the_key() {
    // Whatever answers at the address sends every call on to a second server
    // on another port: the key must not follow.
    let (other, other_stub) = serve().await;
    quiet(&other_stub);
    let app = Router::new().fallback(move |req: Request| {
        let path = req.uri().path().strip_prefix("/redirecting").unwrap_or("/");
        let to = format!("{other}{path}");
        async move { axum::response::Redirect::temporary(&to) }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let redirecting = format!("http://{}/redirecting", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let s = fetch(&redirecting).await;
    // The second server answers, but it never saw the key.
    assert_eq!(s.status, Status::Ok, "{:?}", s.error);
    let seen = other_stub.seen();
    assert_eq!(seen.len(), 6, "{seen:?}");
    assert!(seen.iter().all(|s| s.auth.is_none()), "{seen:?}");
}
