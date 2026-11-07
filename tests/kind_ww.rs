//! The `ww` kind («Wise Wren», Plan 5, task 3), against a stub of Wise Wren's
//! API: the numbers, the status and the needs, what a failing call, a refused
//! key or a malformed answer does, and that the token stays out of every
//! error, reading and log.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use jiff::{SignedDuration, Timestamp};
use mindful_magpie::integrations::{
    self, FetchCtx, FetchError, Format, Level, Snapshot, Status, error_text_key,
};
use serde_json::{Value as Json, json};

const TOKEN: &str = "ww_s3cr3t-Tok3n-9f8e7d6c";

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
// A stub of Wise Wren

/// One request the stub saw.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    query: String,
    auth: Option<String>,
}

#[derive(Clone, Default)]
struct Stub {
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

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

async fn handle(State(stub): State<Stub>, request: Request) -> Response {
    let path = request.uri().path().to_string();
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

/// Starts the stub; the address without a trailing slash.
async fn serve() -> (String, Stub) {
    let stub = Stub::default();
    let app = Router::new().fallback(handle).with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, stub)
}

const SERVERS: &str = "/api/v1/servers";
const UPDATES: &str = "/api/v1/updates";
const RUNS: &str = "/api/v1/runs";

// ---------------------------------------------------------------------------
// Fixtures in Wise Wren's shapes

fn servers(statuses: &[&str]) -> Json {
    json!({ "servers": statuses.iter().enumerate().map(|(i, s)| json!({
        "id": i + 1,
        "name": format!("Server-{}", i + 1),
        "status": s,
        "last_seen_at": "2026-10-23 10:00:00",
        "hostname": "Tower",
        "docker_error": "",
        "mode": "approve",
        "auto_window": null,
        "check_interval_hours": 6,
    })).collect::<Vec<_>>() })
}

fn updates(n: usize) -> Json {
    json!({ "targets": (0..n).map(|i| json!({
        "id": 100 + i,
        "server": { "id": 1, "name": "Server-1" },
        "kind": "compose",
        "name": format!("stack-{i}"),
        "image": "ghcr.io/example/app:latest",
        "status": "update",
        "available": "2/6",
        "release_notes": null,
    })).collect::<Vec<_>>() })
}

/// `hours` ago, the way Wise Wren writes a time (UTC, no zone).
fn ago(hours: i64) -> String {
    Timestamp::now()
        .checked_sub(SignedDuration::from_hours(hours))
        .unwrap()
        .strftime("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// One run: id, target id, status, and when it finished (if it has).
type Run = (i64, i64, &'static str, Option<String>);

/// Newest first, as Wise Wren answers.
fn runs(list: &[Run]) -> Json {
    json!({ "runs": list.iter().map(|(id, target, status, finished)| json!({
        "id": id,
        "server": { "id": 1, "name": "Server-1" },
        "target": { "id": target, "kind": "container", "name": format!("target-{target}") },
        "kind": "update",
        "status": status,
        "detail": "",
        "created_at": ago(40),
        "started_at": finished.as_ref().map(|_| ago(40)),
        "finished_at": finished,
    })).collect::<Vec<_>>() })
}

/// The answers of a quiet Wise Wren: two agents online, nothing to update, one
/// good run.
fn quiet(stub: &Stub) {
    stub.ok(SERVERS, &servers(&["online", "online"]));
    stub.ok(UPDATES, &updates(0));
    stub.ok(RUNS, &runs(&[(5, 1, "succeeded", Some(ago(2)))]));
}

fn ctx(base: &str, secret: Option<&str>) -> FetchCtx {
    FetchCtx {
        http: integrations::build_client(Duration::from_secs(5)),
        settings: json!({ "url": base }),
        secret: secret.map(str::to_string),
    }
}

async fn fetch(base: &str) -> Snapshot {
    integrations::find("ww")
        .expect("the registry lists ww")
        .fetch(&ctx(base, Some(TOKEN)))
        .await
        .expect("a reading, good or bad")
}

fn num(snapshot: &Snapshot, id: &str) -> Option<f64> {
    snapshot.values.get(id).and_then(|v| v.num)
}

// ---------------------------------------------------------------------------
// The kind

#[test]
fn the_kind_describes_itself() {
    let kind = integrations::find("ww").expect("the registry lists ww");
    assert_eq!(kind.kind(), "ww");
    assert_eq!(kind.label_key(), "kind.ww");
    assert_eq!(kind.default_interval(), Duration::from_secs(60));
    assert_eq!(kind.min_interval(), Duration::from_secs(30));
    assert!(kind.url_required());
    let metrics: Vec<_> = kind.metrics().iter().map(|m| (m.id, m.format)).collect();
    assert_eq!(
        metrics,
        [
            ("updates_ready", Format::Number),
            ("agents_online", Format::Number),
            ("agents_total", Format::Number),
            ("last_run", Format::Since),
            ("failed_targets", Format::Number),
        ]
    );
    // The token is the only field, and it is the secret.
    let fields = kind.fields();
    assert_eq!(fields.len(), 1);
    assert!(fields[0].secret);
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
    let kind = integrations::find("ww").unwrap();
    let mut keys = vec![kind.label_key().to_string()];
    for field in kind.fields() {
        keys.push(field.label_key.to_string());
        keys.push(field.help_key.to_string());
    }
    keys.extend(kind.metrics().iter().map(|m| m.label_key.to_string()));
    keys.extend(["need.ww_failed", "need.ww_offline", "fetch_err.auth"].map(str::to_string));
    let mut texts: HashMap<&str, Vec<String>> = HashMap::new();
    for lang in ["en", "nb"] {
        for key in &keys {
            let (section, name) = key.split_once('.').unwrap();
            let text = locale(lang, section)
                .remove(name)
                .unwrap_or_else(|| panic!("{lang}: no text for {key}"));
            assert!(!text.is_empty(), "{lang}: {key} is empty");
            if section == "need" {
                // The chip already shows the service's name.
                assert!(!text.contains("{name}"), "{lang}: {key}: {text}");
                assert!(text.contains("{count}"), "{lang}: {key}: {text}");
            }
            texts.entry(lang).or_default().push(text);
        }
    }
    // The error text is the agreed one, in both languages.
    let auth = keys.iter().position(|k| k == "fetch_err.auth").unwrap();
    assert_eq!(texts["en"][auth], "The key was refused");
    assert_eq!(texts["nb"][auth], "Nøkkelen ble avvist");
    // The form help says where to make the token and which scope.
    let help = keys
        .iter()
        .position(|k| k == "field.ww_secret_help")
        .unwrap();
    assert!(
        texts["en"][help].contains("API tokens"),
        "{}",
        texts["en"][help]
    );
    assert!(texts["en"][help].contains("Read"), "{}", texts["en"][help]);
    assert!(
        texts["nb"][help].contains("API-tokens"),
        "{}",
        texts["nb"][help]
    );
    assert!(texts["nb"][help].contains("Lese"), "{}", texts["nb"][help]);
}

#[test]
fn the_refused_key_text_maps_to_its_translation_key() {
    assert_eq!(FetchError::Auth.to_string(), "The key was refused");
    assert_eq!(FetchError::Auth.text_key(), Some("fetch_err.auth"));
    assert_eq!(
        error_text_key("The key was refused"),
        Some("fetch_err.auth")
    );
}

// ---------------------------------------------------------------------------
// A full set of answers

#[tokio::test]
async fn a_quiet_wise_wren_is_ok() {
    let (base, stub) = serve().await;
    quiet(&stub);
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok, "{:?}", s.error);
    assert_eq!(s.error, None);
    assert!(s.needs.is_empty());
    assert_eq!(num(&s, "updates_ready"), Some(0.0));
    assert_eq!(num(&s, "agents_online"), Some(2.0));
    assert_eq!(num(&s, "agents_total"), Some(2.0));
    assert_eq!(num(&s, "failed_targets"), Some(0.0));
}

#[tokio::test]
async fn a_full_set_of_answers_gives_every_number() {
    let (base, stub) = serve().await;
    stub.ok(
        SERVERS,
        &servers(&["online", "online", "offline", "waiting", "no_token"]),
    );
    stub.ok(UPDATES, &updates(3));
    let finished = ago(1);
    stub.ok(
        RUNS,
        &runs(&[
            (9, 2, "failed", Some(finished.clone())),
            (8, 1, "succeeded", Some(ago(5))),
        ]),
    );
    let s = fetch(&base).await;
    assert_eq!(num(&s, "updates_ready"), Some(3.0));
    assert_eq!(num(&s, "agents_online"), Some(2.0));
    assert_eq!(num(&s, "agents_total"), Some(5.0));
    assert_eq!(num(&s, "failed_targets"), Some(1.0));
    // The newest run's own time (its finish), as Unix seconds.
    let last = num(&s, "last_run").expect("last_run");
    let want = Timestamp::now().as_second() - 3600;
    assert!(
        (last - want as f64).abs() < 5.0,
        "last_run {last}, expected about {want}"
    );
    assert_eq!(s.values.len(), 5);
    assert_eq!(s.error, None);
}

#[tokio::test]
async fn a_run_that_has_not_finished_is_dated_by_its_start_or_creation() {
    let (base, stub) = serve().await;
    stub.ok(SERVERS, &servers(&["online"]));
    stub.ok(UPDATES, &updates(0));
    // `created_at` is 40 hours ago in the fixture; no finish.
    stub.ok(RUNS, &runs(&[(3, 1, "running", None)]));
    let s = fetch(&base).await;
    let last = num(&s, "last_run").expect("last_run");
    let want = Timestamp::now().as_second() - 40 * 3600;
    assert!((last - want as f64).abs() < 5.0, "{last} vs {want}");
    assert_eq!(s.status, Status::Ok);
}

#[tokio::test]
async fn no_runs_yet_leaves_out_last_run_only() {
    let (base, stub) = serve().await;
    stub.ok(SERVERS, &servers(&["online"]));
    stub.ok(UPDATES, &updates(0));
    stub.ok(RUNS, &json!({ "runs": [] }));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok, "{:?}", s.error);
    assert_eq!(num(&s, "last_run"), None);
    assert_eq!(num(&s, "failed_targets"), Some(0.0));
}

#[tokio::test]
async fn three_calls_with_the_token_and_one_bounded_page_of_runs() {
    let (base, stub) = serve().await;
    quiet(&stub);
    fetch(&base).await;
    let mut seen = stub.seen();
    seen.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(seen.len(), 3, "{seen:?}");
    let paths: Vec<_> = seen.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, [RUNS, SERVERS, UPDATES]);
    let bearer = format!("Bearer {TOKEN}");
    for call in &seen {
        assert_eq!(call.auth.as_deref(), Some(bearer.as_str()), "{call:?}");
    }
    // One page, sized for the job, never followed on.
    let runs_call = &seen[0];
    assert_eq!(runs_call.query, "limit=100");
    assert!(seen[1].query.is_empty() && seen[2].query.is_empty());
}

#[tokio::test]
async fn the_address_may_carry_a_path_prefix() {
    let (base, stub) = serve().await;
    quiet(&stub);
    // Wise Wren behind a reverse proxy under a path: the stub only answers
    // the plain paths, so check what was asked.
    let kind = integrations::find("ww").unwrap();
    let c = ctx(&format!("{base}/wren/"), Some(TOKEN));
    let _ = kind.fetch(&c).await;
    let mut paths: Vec<_> = stub.seen().into_iter().map(|s| s.path).collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            "/wren/api/v1/runs",
            "/wren/api/v1/servers",
            "/wren/api/v1/updates"
        ]
    );
}

// ---------------------------------------------------------------------------
// Status

#[tokio::test]
async fn updates_ready_warn() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(UPDATES, &updates(2));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Warn);
    assert_eq!(num(&s, "updates_ready"), Some(2.0));
    // Updates alone are not a need.
    assert!(s.needs.is_empty(), "{:?}", s.needs);
}

#[tokio::test]
async fn an_offline_agent_warns() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(SERVERS, &servers(&["online", "offline"]));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Warn);
    assert_eq!(num(&s, "agents_online"), Some(1.0));
    assert_eq!(num(&s, "agents_total"), Some(2.0));
}

#[tokio::test]
async fn waiting_and_tokenless_servers_are_not_offline() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.ok(SERVERS, &servers(&["online", "waiting", "no_token"]));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok, "{:?}", s.error);
    assert!(s.needs.is_empty());
    assert_eq!(num(&s, "agents_online"), Some(1.0));
    assert_eq!(num(&s, "agents_total"), Some(3.0));
}

#[tokio::test]
async fn a_failed_target_is_bad_even_with_updates_and_offline_agents() {
    let (base, stub) = serve().await;
    stub.ok(SERVERS, &servers(&["online", "offline"]));
    stub.ok(UPDATES, &updates(4));
    stub.ok(RUNS, &runs(&[(7, 1, "rolled_back", Some(ago(1)))]));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    // A failed update is a finding about the data, not a failed fetch.
    assert_eq!(s.error, None);
}

// ---------------------------------------------------------------------------
// Failed targets: the latest run per target

#[tokio::test]
async fn only_the_latest_run_of_a_target_decides() {
    let (base, stub) = serve().await;
    stub.ok(SERVERS, &servers(&["online"]));
    stub.ok(UPDATES, &updates(0));
    stub.ok(
        RUNS,
        &runs(&[
            // Target 1: failed long ago, a success since: fine.
            (20, 1, "succeeded", Some(ago(1))),
            // Target 2: succeeded, then the latest failed: counts.
            (19, 2, "failed", Some(ago(2))),
            // Target 3: the latest was rolled back: counts.
            (18, 3, "rolled_back", Some(ago(3))),
            // Target 4: needs a person: counts.
            (17, 4, "rollback_failed", Some(ago(4))),
            // Target 5: a retry is running after a failure: not yet.
            (16, 5, "running", None),
            // Target 6: a queued retry: not yet.
            (15, 6, "queued", None),
            (14, 1, "failed", Some(ago(6))),
            (13, 2, "succeeded", Some(ago(7))),
            (12, 5, "failed", Some(ago(8))),
            (11, 6, "rolled_back", Some(ago(9))),
            (10, 1, "failed", Some(ago(10))),
        ]),
    );
    let s = fetch(&base).await;
    assert_eq!(num(&s, "failed_targets"), Some(3.0));
    assert_eq!(s.status, Status::Bad);
}

#[tokio::test]
async fn an_older_failed_run_superseded_by_a_success_does_not_count() {
    let (base, stub) = serve().await;
    stub.ok(SERVERS, &servers(&["online"]));
    stub.ok(UPDATES, &updates(0));
    stub.ok(
        RUNS,
        &runs(&[
            (6, 1, "succeeded", Some(ago(1))),
            (5, 1, "failed", Some(ago(2))),
            (4, 1, "rollback_failed", Some(ago(3))),
        ]),
    );
    let s = fetch(&base).await;
    assert_eq!(num(&s, "failed_targets"), Some(0.0));
    assert_eq!(s.status, Status::Ok);
    assert!(s.needs.is_empty());
}

#[tokio::test]
async fn the_order_of_the_list_is_not_trusted() {
    let (base, stub) = serve().await;
    stub.ok(SERVERS, &servers(&["online"]));
    stub.ok(UPDATES, &updates(0));
    // Oldest first: the success has the higher id, so it is the latest.
    stub.ok(
        RUNS,
        &runs(&[
            (4, 1, "failed", Some(ago(3))),
            (5, 1, "succeeded", Some(ago(2))),
        ]),
    );
    let s = fetch(&base).await;
    assert_eq!(num(&s, "failed_targets"), Some(0.0));
}

#[tokio::test]
async fn a_run_that_never_ran_does_not_hide_or_make_a_failure() {
    let (base, stub) = serve().await;
    stub.ok(SERVERS, &servers(&["online"]));
    stub.ok(UPDATES, &updates(0));
    stub.ok(
        RUNS,
        &runs(&[
            // Cancelled, lapsed and refused after a failure: the failure
            // stands. Before one, they are no failure.
            (10, 1, "cancelled", Some(ago(1))),
            (9, 1, "failed", Some(ago(2))),
            (8, 2, "lapsed", Some(ago(3))),
            (7, 3, "refused", Some(ago(4))),
        ]),
    );
    let s = fetch(&base).await;
    assert_eq!(num(&s, "failed_targets"), Some(1.0));
}

#[tokio::test]
async fn an_old_failure_is_left_alone() {
    let (base, stub) = serve().await;
    stub.ok(SERVERS, &servers(&["online"]));
    stub.ok(UPDATES, &updates(0));
    stub.ok(
        RUNS,
        &runs(&[
            // Three days is the limit.
            (2, 1, "failed", Some(ago(24 * 5))),
            (1, 2, "failed", Some(ago(24 * 2))),
        ]),
    );
    let s = fetch(&base).await;
    assert_eq!(num(&s, "failed_targets"), Some(1.0));
}

#[tokio::test]
async fn a_run_without_a_target_is_ignored() {
    let (base, stub) = serve().await;
    stub.ok(SERVERS, &servers(&["online"]));
    stub.ok(UPDATES, &updates(0));
    stub.ok(
        RUNS,
        &json!({ "runs": [
            { "id": 3, "target": null, "status": "failed", "created_at": ago(1) },
            { "id": 2, "status": "failed", "created_at": ago(1) },
        ] }),
    );
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Ok, "{:?}", s.error);
    assert_eq!(num(&s, "failed_targets"), Some(0.0));
}

// ---------------------------------------------------------------------------
// Needs

#[tokio::test]
async fn the_needs_their_keys_levels_scores_and_params() {
    let (base, stub) = serve().await;
    stub.ok(
        SERVERS,
        &servers(&["online", "offline", "offline", "offline"]),
    );
    stub.ok(UPDATES, &updates(1));
    stub.ok(
        RUNS,
        &runs(&[
            (3, 1, "failed", Some(ago(1))),
            (2, 2, "rolled_back", Some(ago(1))),
        ]),
    );
    let s = fetch(&base).await;
    assert_eq!(s.needs.len(), 2, "{:?}", s.needs);
    let failed = s.needs.iter().find(|n| n.key == "ww:failed").unwrap();
    assert_eq!(failed.level, Level::Bad);
    assert_eq!(failed.label_key, "need.ww_failed");
    assert_eq!(failed.score, 2.0);
    assert_eq!(failed.params.get("count").map(String::as_str), Some("2"));
    let offline = s.needs.iter().find(|n| n.key == "ww:offline").unwrap();
    assert_eq!(offline.level, Level::Warn);
    assert_eq!(offline.label_key, "need.ww_offline");
    assert_eq!(offline.score, 3.0);
    assert_eq!(offline.params.get("count").map(String::as_str), Some("3"));
    // Nothing but a count: the chip shows the service's name itself.
    for need in &s.needs {
        assert_eq!(need.params.len(), 1, "{:?}", need.params);
    }
}

#[tokio::test]
async fn a_need_is_absent_when_there_is_nothing_to_need() {
    let (base, stub) = serve().await;
    quiet(&stub);
    assert!(fetch(&base).await.needs.is_empty());
}

#[tokio::test]
async fn names_from_wise_wren_never_reach_the_needs_or_the_error() {
    let (base, stub) = serve().await;
    let hostile = "<script>alert(1)</script>";
    stub.ok(
        SERVERS,
        &json!({ "servers": [
            { "id": 1, "name": hostile, "status": "offline" },
            { "id": 2, "name": "Tower", "status": "online" },
        ] }),
    );
    stub.ok(
        UPDATES,
        &json!({ "targets": [
            { "id": 1, "name": hostile, "image": hostile, "status": "update",
              "server": { "id": 1, "name": hostile } },
        ] }),
    );
    stub.ok(
        RUNS,
        &json!({ "runs": [
            { "id": 1, "status": "failed", "detail": hostile, "created_at": ago(1),
              "finished_at": ago(1), "server": { "id": 1, "name": hostile },
              "target": { "id": 1, "kind": "container", "name": hostile } },
        ] }),
    );
    let s = fetch(&base).await;
    assert_eq!(s.needs.len(), 2);
    let stored = serde_json::to_string(&s).unwrap();
    assert!(!stored.contains("script"), "{stored}");
    assert!(!stored.contains("Tower"), "{stored}");
}

// ---------------------------------------------------------------------------
// One call failing, a refused key, malformed answers

#[tokio::test]
async fn one_call_failing_gives_a_bad_reading_with_fewer_numbers() {
    // (the failing path, the numbers that should still be there)
    let cases = [
        (
            SERVERS,
            ["updates_ready", "failed_targets", "last_run"].as_slice(),
        ),
        (
            UPDATES,
            [
                "agents_online",
                "agents_total",
                "failed_targets",
                "last_run",
            ]
            .as_slice(),
        ),
        (
            RUNS,
            ["agents_online", "agents_total", "updates_ready"].as_slice(),
        ),
    ];
    for (failing, kept) in cases {
        let (base, stub) = serve().await;
        quiet(&stub);
        stub.reply(failing, 500, "boom");
        let s = fetch(&base).await;
        assert_eq!(s.status, Status::Bad, "{failing}");
        assert_eq!(s.error.as_deref(), Some("HTTP 500"), "{failing}");
        let mut have: Vec<_> = s.values.keys().map(String::as_str).collect();
        let mut want: Vec<_> = kept.to_vec();
        have.sort_unstable();
        want.sort_unstable();
        assert_eq!(have, want, "{failing}");
    }
}

#[tokio::test]
async fn a_failing_call_still_lets_the_others_raise_needs() {
    let (base, stub) = serve().await;
    stub.ok(SERVERS, &servers(&["offline"]));
    stub.ok(UPDATES, &updates(0));
    stub.reply(RUNS, 500, "");
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.needs.len(), 1);
    assert_eq!(s.needs[0].key, "ww:offline");
}

#[tokio::test]
async fn every_call_failing_gives_a_bad_reading_without_numbers() {
    let (base, stub) = serve().await;
    for path in [SERVERS, UPDATES, RUNS] {
        stub.reply(path, 503, "");
    }
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("HTTP 503"));
    assert!(s.values.is_empty());
    assert!(s.needs.is_empty());
}

#[tokio::test]
async fn a_missing_endpoint_is_a_bad_reading_not_a_panic() {
    // An old or other server that does not know the routes: 404 on all.
    let (base, _stub) = serve().await;
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("HTTP 404"));
}

#[tokio::test]
async fn an_unreachable_server_is_a_bad_reading() {
    let s = fetch("http://127.0.0.1:1").await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("connection failed"));
}

#[tokio::test]
async fn a_refused_key_says_so_and_no_more() {
    for code in [401, 403] {
        let (base, stub) = serve().await;
        for path in [SERVERS, UPDATES, RUNS] {
            stub.reply(path, code, json!({ "error": "unauthorized" }).to_string());
        }
        let s = fetch(&base).await;
        assert_eq!(s.status, Status::Bad, "{code}");
        assert_eq!(s.error.as_deref(), Some("The key was refused"), "{code}");
        assert_eq!(
            error_text_key(s.error.as_deref().unwrap()),
            Some("fetch_err.auth")
        );
        assert!(s.values.is_empty());
    }
}

#[tokio::test]
async fn a_refusal_in_one_call_beats_another_failure() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.reply(SERVERS, 500, "");
    stub.reply(UPDATES, 401, "");
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("The key was refused"));
    assert!(s.values.contains_key("failed_targets"));
}

#[tokio::test]
async fn malformed_json_is_a_bad_reading_not_a_panic() {
    let bodies = [
        "not json at all",
        "",
        "{\"servers\": [",
        "null",
        "[]",
        "{}",
        "{\"servers\": 5, \"targets\": \"x\", \"runs\": {}}",
        "{\"servers\": [1, 2], \"targets\": [null], \"runs\": [\"a\"]}",
    ];
    for body in bodies {
        for failing in [SERVERS, UPDATES, RUNS] {
            let (base, stub) = serve().await;
            quiet(&stub);
            stub.reply(failing, 200, body);
            let s = fetch(&base).await;
            assert_eq!(s.status, Status::Bad, "{failing}: {body:?}");
            assert_eq!(
                s.error.as_deref(),
                Some("unexpected answer"),
                "{failing}: {body:?}"
            );
            assert!(s.values.len() < 5, "{failing}: {body:?}");
        }
    }
}

#[tokio::test]
async fn all_three_malformed_gives_no_numbers() {
    let (base, stub) = serve().await;
    for path in [SERVERS, UPDATES, RUNS] {
        stub.reply(path, 200, "<html>hello</html>");
    }
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("unexpected answer"));
    assert!(s.values.is_empty());
}

#[tokio::test]
async fn an_oversized_answer_is_refused() {
    let (base, stub) = serve().await;
    quiet(&stub);
    stub.reply(RUNS, 200, " ".repeat(integrations::MAX_BODY + 10));
    let s = fetch(&base).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("response too large"));
}

#[tokio::test]
async fn unknown_fields_and_missing_optional_fields_are_fine() {
    let (base, stub) = serve().await;
    stub.ok(
        SERVERS,
        &json!({ "servers": [{ "status": "online", "something_new": 1 }], "extra": true }),
    );
    stub.ok(UPDATES, &json!({ "targets": [{ "id": 1 }], "next": null }));
    stub.ok(
        RUNS,
        &json!({ "runs": [{ "id": 1, "target": { "id": 1 }, "status": "succeeded" }] }),
    );
    let s = fetch(&base).await;
    assert_eq!(s.error, None);
    assert_eq!(num(&s, "agents_online"), Some(1.0));
    assert_eq!(num(&s, "updates_ready"), Some(1.0));
    // A run with no time at all gives no `last_run`, not a made-up one.
    assert_eq!(num(&s, "last_run"), None);
}

// ---------------------------------------------------------------------------
// Settings

#[tokio::test]
async fn no_token_is_a_settings_problem_and_nothing_is_sent() {
    let (base, stub) = serve().await;
    quiet(&stub);
    let kind = integrations::find("ww").unwrap();
    for secret in [None, Some(String::new())] {
        let c = FetchCtx {
            secret,
            ..ctx(&base, None)
        };
        assert_eq!(kind.fetch(&c).await.unwrap_err(), FetchError::Config);
    }
    assert!(stub.seen().is_empty());
}

#[tokio::test]
async fn an_address_that_cannot_be_used_is_refused_before_any_request() {
    let kind = integrations::find("ww").unwrap();
    for bad in [
        "",
        "not a url",
        "ftp://example.com/",
        "http://user:pw@example.com/",
    ] {
        let c = ctx(bad, Some(TOKEN));
        let err = kind.fetch(&c).await.unwrap_err();
        assert_eq!(err, FetchError::Config, "{bad:?}");
        assert!(!err.to_string().contains(TOKEN));
    }
    let c = FetchCtx {
        settings: json!({}),
        ..ctx("", Some(TOKEN))
    };
    assert_eq!(kind.fetch(&c).await.unwrap_err(), FetchError::Config);
    for forbidden in ["http://169.254.169.254/", "http://[fe80::1]/"] {
        let c = ctx(forbidden, Some(TOKEN));
        assert_eq!(
            kind.fetch(&c).await.unwrap_err(),
            FetchError::Forbidden,
            "{forbidden}"
        );
    }
}

#[tokio::test]
async fn a_token_that_cannot_be_a_header_is_a_settings_problem() {
    let (base, stub) = serve().await;
    quiet(&stub);
    let kind = integrations::find("ww").unwrap();
    let c = ctx(&base, Some("bad\r\ntoken"));
    assert_eq!(kind.fetch(&c).await.unwrap_err(), FetchError::Config);
    assert!(stub.seen().is_empty());
}

// ---------------------------------------------------------------------------
// The token stays secret

#[tokio::test]
async fn the_token_is_in_no_reading_error_or_log() {
    let _ = log_buffer();
    let mut readings = Vec::new();

    // A good set, a failing call, a refusal, malformed, unreachable.
    let (base, stub) = serve().await;
    quiet(&stub);
    readings.push(fetch(&base).await);
    stub.reply(RUNS, 500, format!("{{\"echo\": \"Bearer {TOKEN}\"}}"));
    readings.push(fetch(&base).await);
    for path in [SERVERS, UPDATES, RUNS] {
        stub.reply(path, 401, format!("Bearer {TOKEN}"));
    }
    readings.push(fetch(&base).await);
    for path in [SERVERS, UPDATES, RUNS] {
        stub.reply(path, 200, format!("{{\"servers\": \"{TOKEN}\"}}"));
    }
    readings.push(fetch(&base).await);
    readings.push(fetch("http://127.0.0.1:1").await);

    for reading in &readings {
        let stored = serde_json::to_string(reading).unwrap();
        assert!(!stored.contains(TOKEN), "{stored}");
        assert!(!stored.contains("Bearer"), "{stored}");
        assert!(!format!("{reading:?}").contains(TOKEN));
    }

    let c = ctx(&base, Some(TOKEN));
    assert!(!format!("{c:?}").contains(TOKEN));
    for forbidden in ["http://169.254.169.254/", "ftp://x/"] {
        let err = integrations::find("ww")
            .unwrap()
            .fetch(&ctx(forbidden, Some(TOKEN)))
            .await
            .unwrap_err();
        assert!(!err.to_string().contains(TOKEN));
        assert!(!format!("{err:?}").contains(TOKEN));
    }

    let log = logged();
    assert!(!log.contains(TOKEN), "the token reached the log");
    assert!(
        !log.contains("Bearer"),
        "an Authorization value reached the log"
    );
}

#[tokio::test]
async fn a_redirect_to_another_origin_does_not_carry_the_token() {
    // Wise Wren (or whatever answers at the address) sends every call on to a
    // second server on another port: the token must not follow.
    let (other, other_stub) = serve().await;
    quiet(&other_stub);
    let app = Router::new().fallback(move |req: Request| {
        let to = format!("{other}{}", req.uri().path());
        async move { axum::response::Redirect::temporary(&to) }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let redirecting = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let s = fetch(&redirecting).await;
    // The second server answers, but it never saw the key.
    assert_eq!(s.status, Status::Ok, "{:?}", s.error);
    let seen = other_stub.seen();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(seen.iter().all(|s| s.auth.is_none()), "{seen:?}");
}
