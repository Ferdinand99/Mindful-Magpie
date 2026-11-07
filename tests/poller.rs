//! The background poller (Plan 4, task 2): when it fetches, how it backs off,
//! what it stores and announces, and that a slow, hostile or secret-holding
//! service stays contained.
//!
//! Most tests run on tokio's paused clock with a stub `Integration` (a kind
//! that is not in the registry; the poller takes its kinds through
//! `Options::kinds`). The tests that use a real stub web server run in real
//! time, because a paused clock would fire the HTTP client's timeouts while
//! the runtime waits for the network.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Redirect;
use axum::routing::get;
use common::App;
use mindful_magpie::integrations::poller::{FETCH_DEADLINE, Options, Poller, PollerHandle};
use mindful_magpie::integrations::{
    self, BoxFuture, FetchCtx, FetchError, Field, Integration, MetricDef, Snapshot, Status, Value,
};
use mindful_magpie::services::{SecretChange, ServiceEvent, ServiceInput, Visibility, store};
use serde_json::{Value as Json, json};
use tokio::time::Instant;

const SECRET: &str = "s3cr3t-Tok3n-9f8e7d6c";
const USERINFO_PASSWORD: &str = "hunter2-pw-4711";

// ---------------------------------------------------------------------------
// Capturing what the poller logs

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
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("no other subscriber in this test binary");
    });
    buffer
}

fn logged() -> String {
    String::from_utf8_lossy(&log_buffer().0.lock().unwrap()).into_owned()
}

// ---------------------------------------------------------------------------
// The stub kind

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// An ok reading.
    Ok,
    /// `Ok` with a bad reading that says why (what the http kind does).
    Bad,
    /// `Err`: no reading could be made.
    Err,
    /// Never answers.
    Hang,
}

#[derive(Clone)]
struct Call {
    at: Instant,
    marker: String,
}

struct Stub {
    kind: &'static str,
}

/// A kind called `stub`, and one that answers to `http`, for the rule that is
/// the http kind's (bad twice in a row) without waiting 15 real seconds.
static STUB: Stub = Stub { kind: "stub" };
static STUB_AS_HTTP: Stub = Stub { kind: "http" };

static MODES: Mutex<Vec<(String, Mode)>> = Mutex::new(Vec::new());
static CALLS: Mutex<Vec<(String, Call)>> = Mutex::new(Vec::new());

/// Every test names its own tag, so the tests do not see each other.
fn set_mode(tag: &str, mode: Mode) {
    let mut modes = MODES.lock().unwrap();
    modes.retain(|(t, _)| t != tag);
    modes.push((tag.to_string(), mode));
}

fn mode_of(tag: &str) -> Mode {
    MODES
        .lock()
        .unwrap()
        .iter()
        .find(|(t, _)| t == tag)
        .map_or(Mode::Ok, |(_, m)| *m)
}

fn calls(tag: &str) -> Vec<Call> {
    CALLS
        .lock()
        .unwrap()
        .iter()
        .filter(|(t, _)| t == tag)
        .map(|(_, c)| c.clone())
        .collect()
}

static NO_FIELDS: [Field; 0] = [];
static NO_METRICS: [MetricDef; 0] = [];

impl Integration for Stub {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn label_key(&self) -> &'static str {
        "kind.stub"
    }
    fn fields(&self) -> &'static [Field] {
        &NO_FIELDS
    }
    fn metrics(&self) -> &'static [MetricDef] {
        &NO_METRICS
    }
    fn default_interval(&self) -> Duration {
        Duration::from_secs(60)
    }
    fn min_interval(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn fetch<'a>(&'a self, ctx: &'a FetchCtx) -> BoxFuture<'a, Result<Snapshot, FetchError>> {
        Box::pin(async move {
            let tag = ctx.settings["tag"].as_str().unwrap_or("").to_string();
            let marker = ctx.settings["marker"].as_str().unwrap_or("").to_string();
            let n = {
                let mut all = CALLS.lock().unwrap();
                all.push((
                    tag.clone(),
                    Call {
                        at: Instant::now(),
                        marker: marker.clone(),
                    },
                ));
                all.iter().filter(|(t, _)| *t == tag).count()
            };
            match mode_of(&tag) {
                Mode::Hang => std::future::pending().await,
                Mode::Err => Err(FetchError::Config),
                Mode::Bad => Ok(Snapshot::failed(&FetchError::Connect)),
                Mode::Ok => {
                    let mut snapshot = Snapshot::new(Status::Ok);
                    snapshot.values.insert("n".into(), Value::num(n as f64));
                    snapshot.values.insert("marker".into(), Value::text(marker));
                    Ok(snapshot)
                }
            }
        })
    }
}

fn stub_kinds(kind: &str) -> Option<&'static dyn Integration> {
    match kind {
        "stub" => Some(&STUB),
        other => integrations::find(other),
    }
}

/// Like [`stub_kinds`], but the stub answers to `http`.
fn stub_as_http_kinds(kind: &str) -> Option<&'static dyn Integration> {
    match kind {
        "http" => Some(&STUB_AS_HTTP),
        other => integrations::find(other),
    }
}

// ---------------------------------------------------------------------------
// Helpers

async fn add_service(
    app: &App,
    kind: &'static str,
    name: &'static str,
    url: Option<String>,
    settings: Json,
    interval_s: i64,
) -> i64 {
    let settings = settings.to_string();
    app.magpie
        .core
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO services
                   (kind, name, url, settings, secret, interval_s, visibility, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5, 'admin', '2026-11-07T00:00:00Z', '2026-11-07T00:00:00Z')",
                rusqlite::params![kind, name, url, settings, interval_s],
            )?;
            Ok(c.last_insert_rowid())
        })
        .await
        .unwrap()
}

async fn add_stub(app: &App, name: &'static str, tag: &str, marker: &str, interval_s: i64) -> i64 {
    add_service(
        app,
        "stub",
        name,
        None,
        json!({ "tag": tag, "marker": marker }),
        interval_s,
    )
    .await
}

fn start_with(
    app: &App,
    kinds: fn(&str) -> Option<&'static dyn Integration>,
    spread: u64,
) -> PollerHandle {
    log_buffer();
    Poller::start_with(
        app.magpie.core.clone(),
        integrations::client().clone(),
        &app.magpie.service_events,
        Options {
            kinds,
            spread: Duration::from_secs(spread),
        },
    )
}

/// Lets tasks and the database threads run without moving the paused clock
/// (a task that yields keeps the runtime from auto-advancing it).
async fn settle() {
    for _ in 0..4 {
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(3));
    }
}

/// Moves the paused clock in steps of `step` seconds, up to `max` seconds,
/// until `done`.
async fn advance_until(what: &str, step: u64, max: u64, done: impl Fn() -> bool) {
    settle().await;
    let mut spent = 0;
    while !done() {
        assert!(spent < max, "gave up waiting for {what} after {max} s");
        tokio::time::advance(Duration::from_secs(step)).await;
        spent += step;
        settle().await;
    }
}

/// Waits (without moving the clock) for a snapshot of `id` that `ok` accepts.
async fn snapshot_where(
    poller: &PollerHandle,
    id: i64,
    ok: impl Fn(&Snapshot) -> bool,
) -> Arc<Snapshot> {
    for _ in 0..1000 {
        if let Some(s) = poller.snapshot(id)
            && ok(&s)
        {
            return s;
        }
        settle().await;
    }
    panic!("no matching snapshot of service {id}");
}

fn secs(d: tokio::time::Instant, earlier: tokio::time::Instant) -> f64 {
    d.duration_since(earlier).as_secs_f64()
}

/// `gap` is within the jitter (±10 %) of `nominal`, plus `slack` for the step
/// the clock moves in.
fn assert_gap(what: &str, gap: f64, nominal: f64, slack: f64) {
    assert!(
        gap >= nominal * 0.9 - slack && gap <= nominal * 1.1 + slack,
        "{what}: {gap} s is not about {nominal} s"
    );
}

async fn stored_json(app: &App, id: i64) -> Option<String> {
    app.magpie
        .core
        .with_conn(move |c| {
            use rusqlite::OptionalExtension as _;
            c.query_row(
                "SELECT json FROM service_snapshots WHERE service_id = ?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// Schedule

#[tokio::test(flavor = "current_thread")]
async fn a_service_is_fetched_on_start_and_then_every_interval() {
    let app = App::new().await;
    tokio::time::pause();
    let id = add_stub(&app, "Interval", "t_interval", "a", 60).await;
    let started = Instant::now();
    let poller = start_with(&app, stub_kinds, 5);
    settle().await;
    advance_until("the first fetch", 1, 7, || calls("t_interval").len() == 1).await;
    advance_until("the second fetch", 1, 80, || calls("t_interval").len() == 2).await;
    advance_until("the third fetch", 1, 80, || calls("t_interval").len() == 3).await;
    let c = calls("t_interval");
    assert!(
        secs(c[0].at, started) <= 6.0,
        "the first fetch is staggered over the first 5 s, not later"
    );
    assert_gap("1st to 2nd", secs(c[1].at, c[0].at), 60.0, 1.5);
    assert_gap("2nd to 3rd", secs(c[2].at, c[1].at), 60.0, 1.5);
    let s = snapshot_where(&poller, id, |s| s.values.get("n") == Some(&Value::num(3.0))).await;
    assert_eq!(s.status, Status::Ok);
}

#[tokio::test(flavor = "current_thread")]
async fn a_failing_fetch_backs_off_twofold_to_ten_minutes_and_resets_on_success() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_backoff", Mode::Err);
    let id = add_stub(&app, "Backoff", "t_backoff", "", 150).await;
    let poller = start_with(&app, stub_kinds, 0);
    advance_until("fetch 1", 5, 10, || calls("t_backoff").len() == 1).await;
    // The failure is a bad reading with a short reason, not a missing one.
    let s = snapshot_where(&poller, id, |_| true).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("settings not valid"));
    advance_until("fetch 2", 5, 400, || calls("t_backoff").len() == 2).await;
    advance_until("fetch 3", 5, 700, || calls("t_backoff").len() == 3).await;
    advance_until("fetch 4", 5, 700, || calls("t_backoff").len() == 4).await;
    set_mode("t_backoff", Mode::Ok);
    advance_until("fetch 5", 5, 700, || calls("t_backoff").len() == 5).await;
    advance_until("fetch 6", 5, 700, || calls("t_backoff").len() == 6).await;
    let c = calls("t_backoff");
    // 2 x 150, 4 x 150, then 8 x 150 is capped at 10 minutes.
    assert_gap("after one failure", secs(c[1].at, c[0].at), 300.0, 5.0);
    assert_gap("after two failures", secs(c[2].at, c[1].at), 600.0, 5.0);
    assert_gap("capped", secs(c[3].at, c[2].at), 600.0, 5.0);
    // Fetch 4 failed too (the mode changed after it was made), so fetch 5 is
    // still late; fetch 5 succeeded, so the next one is on time again.
    assert_gap("the last backoff", secs(c[4].at, c[3].at), 600.0, 5.0);
    assert_gap("reset", secs(c[5].at, c[4].at), 150.0, 5.0);
}

#[tokio::test(flavor = "current_thread")]
async fn one_hanging_service_does_not_delay_another() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_hang", Mode::Hang);
    let hung = add_stub(&app, "Hangs", "t_hang", "", 10).await;
    let fine = add_stub(&app, "Fine", "t_fine", "", 10).await;
    let poller = start_with(&app, stub_kinds, 0);
    advance_until("three fetches of the fine one", 1, 25, || {
        calls("t_fine").len() >= 3
    })
    .await;
    assert_eq!(calls("t_hang").len(), 1, "the hanging fetch is still out");
    assert!(poller.snapshot(hung).is_none());
    assert!(poller.snapshot(fine).is_some());
    // A fetch that never ends is given up after a deadline and recorded.
    advance_until("the deadline", 1, FETCH_DEADLINE.as_secs() + 5, || {
        poller.snapshot(hung).is_some()
    })
    .await;
    let s = poller.snapshot(hung).unwrap();
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("timeout"));
}

// ---------------------------------------------------------------------------
// Storing and announcing

#[tokio::test(flavor = "current_thread")]
async fn a_snapshot_is_stored_and_loaded_on_restart_before_the_first_fetch() {
    let app = App::new().await;
    tokio::time::pause();
    let id = add_stub(&app, "Stored", "t_stored", "kept", 60).await;
    let poller = start_with(&app, stub_kinds, 0);
    advance_until("the first fetch", 1, 5, || calls("t_stored").len() == 1).await;
    let first = snapshot_where(&poller, id, |_| true).await;
    // One row, holding the reading.
    let stored = stored_json(&app, id).await.expect("a stored snapshot");
    assert!(stored.contains("kept"), "{stored}");
    assert_eq!(app.count("SELECT COUNT(*) FROM service_snapshots").await, 1);
    drop(poller);
    settle().await;

    // A new poller, whose first fetch is a long way off.
    let poller = start_with(&app, stub_kinds, 1000);
    settle().await;
    let loaded = poller.snapshot(id).expect("the stored snapshot is loaded");
    assert_eq!(loaded.fetched_at, first.fetched_at, "its own time, not now");
    assert_eq!(*loaded, *first);
    assert_eq!(calls("t_stored").len(), 1, "nothing has been fetched yet");
    // And then it is replaced by a fresh one.
    advance_until("a fetch after the restart", 20, 1100, || {
        calls("t_stored").len() == 2
    })
    .await;
    let fresh = snapshot_where(&poller, id, |s| s.values.get("n") == Some(&Value::num(2.0))).await;
    assert_eq!(fresh.status, Status::Ok);
    assert_eq!(app.count("SELECT COUNT(*) FROM service_snapshots").await, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn a_broadcast_goes_out_for_every_new_snapshot() {
    let app = App::new().await;
    tokio::time::pause();
    let a = add_stub(&app, "A", "t_bc_a", "", 10).await;
    let b = add_stub(&app, "B", "t_bc_b", "", 10).await;
    let poller = start_with(&app, stub_kinds, 0);
    let mut rx = poller.subscribe();
    advance_until("three fetches each", 1, 40, || {
        calls("t_bc_a").len() >= 3 && calls("t_bc_b").len() >= 3
    })
    .await;
    snapshot_where(&poller, a, |s| s.values.get("n") == Some(&Value::num(3.0))).await;
    snapshot_where(&poller, b, |s| s.values.get("n") == Some(&Value::num(3.0))).await;
    let (mut for_a, mut for_b) = (0, 0);
    while let Ok(id) = rx.try_recv() {
        if id == a {
            for_a += 1;
        } else if id == b {
            for_b += 1;
        } else {
            panic!("a broadcast for service {id}");
        }
    }
    assert_eq!((for_a, for_b), (3, 3), "one per snapshot");
}

// ---------------------------------------------------------------------------
// Changes while it runs

#[tokio::test(flavor = "current_thread")]
async fn an_update_fetches_again_at_once_with_the_new_settings() {
    let app = App::new().await;
    tokio::time::pause();
    let id = add_stub(&app, "Changing", "t_update", "old", 3600).await;
    let poller = start_with(&app, stub_kinds, 0);
    advance_until("the first fetch", 1, 5, || calls("t_update").len() == 1).await;
    snapshot_where(&poller, id, |_| true).await;

    app.exec(format!(
        "UPDATE services SET settings = '{}' WHERE id = {id}",
        json!({ "tag": "t_update", "marker": "new" })
    ))
    .await;
    app.magpie
        .service_events
        .send(ServiceEvent::Updated(id))
        .unwrap();
    // No clock movement: the fetch is at once, not at the next interval.
    let s = snapshot_where(&poller, id, |s| {
        s.values.get("marker") == Some(&Value::text("new"))
    })
    .await;
    assert_eq!(s.status, Status::Ok);
    let c = calls("t_update");
    assert_eq!(c.len(), 2);
    assert_eq!(c[1].marker, "new");
    assert!(secs(c[1].at, c[0].at) < 1.0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_new_service_is_fetched_at_once_without_the_start_up_stagger() {
    let app = App::new().await;
    tokio::time::pause();
    let poller = start_with(&app, stub_kinds, 1000);
    settle().await;
    let id = add_stub(&app, "Added", "t_created", "", 60).await;
    app.magpie
        .service_events
        .send(ServiceEvent::Created(id))
        .unwrap();
    snapshot_where(&poller, id, |_| true).await;
    assert_eq!(calls("t_created").len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn a_deleted_service_stops_and_its_row_is_gone() {
    let app = App::new().await;
    tokio::time::pause();
    let id = add_stub(&app, "Doomed", "t_delete", "", 10).await;
    let other = add_stub(&app, "Stays", "t_stays", "", 10).await;
    let poller = start_with(&app, stub_kinds, 0);
    advance_until("both fetched", 1, 5, || {
        calls("t_delete").len() == 1 && calls("t_stays").len() == 1
    })
    .await;
    snapshot_where(&poller, id, |_| true).await;
    assert!(stored_json(&app, id).await.is_some());

    assert!(store::delete(&app.magpie.core, id).await.unwrap());
    app.magpie
        .service_events
        .send(ServiceEvent::Deleted(id))
        .unwrap();
    settle().await;
    assert!(poller.snapshot(id).is_none());
    assert!(stored_json(&app, id).await.is_none());
    let before = calls("t_delete").len();
    advance_until("the other one runs on", 1, 60, || {
        calls("t_stays").len() >= 5
    })
    .await;
    assert_eq!(calls("t_delete").len(), before, "the task has stopped");
    assert!(poller.snapshot(id).is_none());
    assert!(poller.snapshot(other).is_some());
    assert_eq!(app.count("SELECT COUNT(*) FROM service_snapshots").await, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn reload_and_remove_work_without_an_event() {
    let app = App::new().await;
    tokio::time::pause();
    let poller = start_with(&app, stub_kinds, 1000);
    settle().await;
    let id = add_stub(&app, "Handle", "t_handle", "", 60).await;
    poller.reload(id);
    snapshot_where(&poller, id, |_| true).await;
    poller.remove(id);
    settle().await;
    assert!(poller.snapshot(id).is_none());
}

// ---------------------------------------------------------------------------
// "Trenger deg" after two bad fetches in a row

#[tokio::test(flavor = "current_thread")]
async fn two_bad_fetches_in_a_row_make_a_need_and_a_good_one_clears_it() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_need", Mode::Bad);
    let id = add_service(&app, "http", "Needy", None, json!({ "tag": "t_need" }), 10).await;
    let poller = start_with(&app, stub_as_http_kinds, 0);
    advance_until("fetch 1", 1, 5, || calls("t_need").len() == 1).await;
    let first = snapshot_where(&poller, id, |_| true).await;
    assert_eq!(first.status, Status::Bad);
    assert!(first.needs.is_empty(), "one bad fetch is not yet a need");
    // Backed off 2x after the failure.
    advance_until("fetch 2", 1, 30, || calls("t_need").len() == 2).await;
    let second = snapshot_where(&poller, id, |s| !s.needs.is_empty()).await;
    assert_eq!(second.needs.len(), 1);
    let need = &second.needs[0];
    assert_eq!(need.key, "down");
    assert_eq!(need.label_key, "need.http_down");
    assert_eq!(need.level, integrations::Level::Bad);
    assert_eq!(need.params.get("name").map(String::as_str), Some("Needy"));

    set_mode("t_need", Mode::Ok);
    advance_until("fetch 3", 1, 60, || calls("t_need").len() == 3).await;
    let third = snapshot_where(&poller, id, |s| s.status == Status::Ok).await;
    assert!(third.needs.is_empty());

    // Bad, good, bad again: not two in a row.
    set_mode("t_need", Mode::Bad);
    advance_until("fetch 4", 1, 30, || calls("t_need").len() == 4).await;
    let fourth = snapshot_where(&poller, id, |s| s.status == Status::Bad).await;
    assert!(fourth.needs.is_empty(), "the streak started over");
}

#[tokio::test(flavor = "current_thread")]
async fn an_error_counts_as_a_bad_fetch_for_the_need() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_need_err", Mode::Err);
    let id = add_service(
        &app,
        "http",
        "Erring",
        None,
        json!({ "tag": "t_need_err" }),
        10,
    )
    .await;
    let poller = start_with(&app, stub_as_http_kinds, 0);
    advance_until("two fetches", 1, 40, || calls("t_need_err").len() == 2).await;
    let s = snapshot_where(&poller, id, |s| !s.needs.is_empty()).await;
    assert_eq!(s.needs[0].key, "down");
}

#[tokio::test(flavor = "current_thread")]
async fn a_kind_without_the_rule_gets_no_down_need() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_no_rule", Mode::Bad);
    let id = add_stub(&app, "NoRule", "t_no_rule", "", 10).await;
    let poller = start_with(&app, stub_kinds, 0);
    advance_until("three fetches", 1, 100, || calls("t_no_rule").len() == 3).await;
    let s = snapshot_where(&poller, id, |_| true).await;
    assert!(s.needs.is_empty());
}

// ---------------------------------------------------------------------------
// A stub web server: hostile answers and a secret

async fn needs_bearer(headers: HeaderMap) -> StatusCode {
    let sent = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if sent == Some(format!("Bearer {SECRET}").as_str()) {
        StatusCode::OK
    } else {
        StatusCode::UNAUTHORIZED
    }
}

async fn hop(axum::extract::Path(n): axum::extract::Path<u32>) -> axum::response::Response {
    use axum::response::IntoResponse;
    if n == 0 {
        "arrived".into_response()
    } else {
        Redirect::temporary(&format!("/hop/{}", n - 1)).into_response()
    }
}

/// Serves on an ephemeral loopback port; the base URL without a trailing slash.
async fn stub_server() -> String {
    let app = Router::new()
        .route("/ok", get(|| async { "hello" }))
        .route("/big", get(|| async { vec![b'a'; 10 * 1024 * 1024] }))
        .route("/hop/{n}", get(hop))
        .route(
            "/meta",
            get(|| async { Redirect::temporary("http://169.254.169.254/latest/meta-data/") }),
        )
        .route("/needs-bearer", get(needs_bearer));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// Real-time wait for a snapshot of `id`.
async fn real_snapshot(poller: &PollerHandle, id: i64) -> Arc<Snapshot> {
    for _ in 0..300 {
        if let Some(s) = poller.snapshot(id) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no snapshot of service {id} after 15 s");
}

#[tokio::test]
async fn hostile_answers_give_a_bad_snapshot_with_a_reason_and_no_panic() {
    let app = App::new().await;
    let base = stub_server().await;
    let ok = add_service(
        &app,
        "http",
        "Fine",
        Some(format!("{base}/ok")),
        json!({}),
        60,
    )
    .await;
    let big = add_service(
        &app,
        "http",
        "Big",
        Some(format!("{base}/big")),
        json!({}),
        60,
    )
    .await;
    let meta = add_service(
        &app,
        "http",
        "Meta",
        Some(format!("{base}/meta")),
        json!({}),
        60,
    )
    .await;
    let hops = add_service(
        &app,
        "http",
        "Hops",
        Some(format!("{base}/hop/5")),
        json!({}),
        60,
    )
    .await;
    let direct = add_service(
        &app,
        "http",
        "Direct",
        Some("http://169.254.169.254/latest/".into()),
        json!({}),
        60,
    )
    .await;
    let poller = start_with(&app, stub_kinds, 0);

    let s = real_snapshot(&poller, ok).await;
    assert_eq!(s.status, Status::Ok, "{s:?}");
    for (id, error) in [
        (big, "response too large"),
        (meta, "address not allowed"),
        (hops, "too many redirects"),
        (direct, "address not allowed"),
    ] {
        let s = real_snapshot(&poller, id).await;
        assert_eq!(s.status, Status::Bad, "service {id}: {s:?}");
        assert_eq!(s.error.as_deref(), Some(error), "service {id}");
    }
    // Nothing panicked: the log has no panic line, and the good one is still
    // fetched after the others failed.
    assert!(!logged().contains("panicked"));
    assert_eq!(app.count("SELECT COUNT(*) FROM service_snapshots").await, 5);
}

#[tokio::test]
async fn no_secret_and_no_userinfo_reaches_a_log_a_snapshot_or_the_database() {
    let app = App::new().await;
    let base = stub_server().await;
    // Through the store, so the secret is sealed as in the real thing.
    let id = store::create(
        &app.magpie.core,
        &ServiceInput {
            kind: "http".into(),
            name: "Guarded".into(),
            url: Some(format!("{base}/needs-bearer")),
            settings: json!({}),
            secret: SecretChange::Replace(SECRET.into()),
            interval_s: 60,
            visibility: Visibility::Admin,
            users: vec![],
        },
    )
    .await
    .unwrap();
    // An address with a password in it can't be stored by the pages; a row
    // that has one anyway must not end up in a log line.
    let userinfo = add_service(
        &app,
        "http",
        "Userinfo",
        Some(format!(
            "http://admin:{USERINFO_PASSWORD}@{}/ok",
            base.trim_start_matches("http://")
        )),
        json!({}),
        60,
    )
    .await;
    // And a secret that can't be a header value.
    let broken = store::create(
        &app.magpie.core,
        &ServiceInput {
            kind: "http".into(),
            name: "Broken".into(),
            url: Some(format!("{base}/ok")),
            settings: json!({}),
            secret: SecretChange::Replace("bad\u{1}token".into()),
            interval_s: 60,
            visibility: Visibility::Admin,
            users: vec![],
        },
    )
    .await;
    let poller = start_with(&app, stub_kinds, 0);

    let s = real_snapshot(&poller, id).await;
    assert_eq!(s.status, Status::Ok, "the secret was sent: {s:?}");
    let u = real_snapshot(&poller, userinfo).await;
    assert_eq!(u.status, Status::Bad);
    assert_eq!(u.error.as_deref(), Some("settings not valid"));
    let mut shown = vec![format!("{s:?}"), format!("{u:?}")];
    if let Ok(broken) = broken {
        shown.push(format!("{:?}", real_snapshot(&poller, broken).await));
    }
    for row_id in [id, userinfo] {
        shown.push(stored_json(&app, row_id).await.unwrap());
    }
    shown.push(logged());
    for text in shown {
        for needle in [SECRET, USERINFO_PASSWORD, "bad\u{1}token", "Bearer "] {
            assert!(!text.contains(needle), "{needle:?} leaked into: {text}");
        }
    }
    // The capture works: the poller does say what it is doing.
    assert!(logged().contains("poller"), "{}", logged());
}
