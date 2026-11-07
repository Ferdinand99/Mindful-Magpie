//! The background poller (Plan 4, task 2): when it fetches, how it backs off,
//! what it stores and announces, and that a slow, hostile or secret-holding
//! service stays contained.
//!
//! Most tests run on tokio's paused clock with a stub `Integration` (a kind
//! that is not in the registry; the poller takes its kinds through
//! `Options::kinds`). The test waits on events (a stub fetch was made, a
//! `Change` was announced) and the runtime moves the paused clock on by itself
//! when everything is waiting for a timer, so the times the stub records are
//! exact. (A database call on a blocking thread keeps the clock from moving
//! until it is done.) The tests that use a real stub web server run in real
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
use jiff::Timestamp;
use mindful_magpie::integrations::poller::{
    CONFIG_KEY, Change, DOWN_KEY, FETCH_DEADLINE, Options, Poller, PollerHandle,
};
use mindful_magpie::integrations::{
    self, BoxFuture, FetchCtx, FetchError, Field, Integration, Level, MetricDef, Need, Snapshot,
    Status, Value,
};
use mindful_magpie::services::{SecretChange, ServiceEvent, ServiceInput, Visibility, store};
use serde_json::{Value as Json, json};
use tokio::sync::Notify;
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
            .with_writer(move || writer.clone());
        // Also bridges the `log` records of reqwest.
        subscriber
            .try_init()
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
    /// `Err(Config)`: no reading could be made, the settings are no good.
    Err,
    /// `Err(Connect)`.
    ErrConnect,
    /// `Err(SecretUnreadable)`.
    ErrSecret,
    ErrAuth,
    /// Never answers.
    Hang,
    /// Answers when the test opens the gate of its tag (see [`gate`]).
    Block,
    /// Panics.
    Panic,
}

#[derive(Clone)]
struct Call {
    at: Instant,
    marker: String,
}

struct Stub {
    kind: &'static str,
    min_interval: u64,
}

/// A kind called `stub`, one that answers to `http` (for the rule that is the
/// http kind's, bad twice in a row, without waiting 15 real seconds) and one
/// with a minimum interval of 10 s.
static STUB: Stub = Stub {
    kind: "stub",
    min_interval: 1,
};
static STUB_AS_HTTP: Stub = Stub {
    kind: "http",
    min_interval: 1,
};
static STUB_MIN10: Stub = Stub {
    kind: "stub10",
    min_interval: 10,
};

static MODES: Mutex<Vec<(String, Mode)>> = Mutex::new(Vec::new());
static CALLS: Mutex<Vec<(String, Call)>> = Mutex::new(Vec::new());
static GATES: Mutex<Vec<(String, Arc<Notify>)>> = Mutex::new(Vec::new());
/// Woken (all waiters) at every stub fetch, in any test.
static CALLED: Notify = Notify::const_new();

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

/// The gate of `tag`: a fetch in [`Mode::Block`] waits on it.
fn gate(tag: &str) -> Arc<Notify> {
    let mut gates = GATES.lock().unwrap();
    if let Some((_, g)) = gates.iter().find(|(t, _)| t == tag) {
        return g.clone();
    }
    let g = Arc::new(Notify::new());
    gates.push((tag.to_string(), g.clone()));
    g
}

static NO_FIELDS: [Field; 0] = [];
static NO_METRICS: [MetricDef; 0] = [];

fn ok_snapshot(n: usize, marker: String) -> Snapshot {
    let mut snapshot = Snapshot::new(Status::Ok);
    snapshot.values.insert("n".into(), Value::num(n as f64));
    snapshot.values.insert("marker".into(), Value::text(marker));
    snapshot
}

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
        Duration::from_secs(self.min_interval)
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
            // The mode is read as the call is made; the test learns of the
            // call only after that.
            let mode = mode_of(&tag);
            CALLED.notify_waiters();
            match mode {
                Mode::Hang => std::future::pending().await,
                Mode::Block => {
                    gate(&tag).notified().await;
                    Ok(ok_snapshot(n, marker))
                }
                Mode::Panic => panic!("the stub kind panics"),
                Mode::Err => Err(FetchError::Config),
                Mode::ErrConnect => Err(FetchError::Connect),
                Mode::ErrSecret => Err(FetchError::SecretUnreadable),
                Mode::ErrAuth => Err(FetchError::Auth),
                Mode::Bad => Ok(Snapshot::failed(&FetchError::Connect)),
                Mode::Ok => Ok(ok_snapshot(n, marker)),
            }
        })
    }
}

fn stub_kinds(kind: &str) -> Option<&'static dyn Integration> {
    match kind {
        "stub" => Some(&STUB),
        "stub10" => Some(&STUB_MIN10),
        other => integrations::find(other),
    }
}

/// Like [`stub_kinds`], but the stub answers to `http`.
fn stub_as_http_kinds(kind: &str) -> Option<&'static dyn Integration> {
    match kind {
        "http" => Some(&STUB_AS_HTTP),
        other => stub_kinds(other),
    }
}

// ---------------------------------------------------------------------------
// Helpers

async fn add_service(
    app: &App,
    kind: &'static str,
    name: &str,
    url: Option<String>,
    settings: Json,
    interval_s: i64,
) -> i64 {
    let settings = settings.to_string();
    let name = name.to_string();
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

async fn add_stub(app: &App, name: &str, tag: &str, marker: &str, interval_s: i64) -> i64 {
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

/// Long enough that the paused clock reaches anything the test waits for;
/// a wait that never ends fails (quickly, the clock runs on by itself) at this.
const LONG: Duration = Duration::from_secs(100_000);

/// Waits until the stub has been fetched `n` times with `tag`. The runtime
/// moves the paused clock on while it waits.
async fn wait_calls(tag: &str, n: usize) {
    loop {
        if calls(tag).len() >= n {
            return;
        }
        tokio::time::timeout(LONG, CALLED.notified())
            .await
            .unwrap_or_else(|_| panic!("gave up waiting for fetch {n} of {tag}"));
    }
}

/// Waits for a snapshot of `id` that `ok` accepts.
async fn wait_snapshot(
    poller: &PollerHandle,
    id: i64,
    ok: impl Fn(&Snapshot) -> bool,
) -> Arc<Snapshot> {
    // Subscribed before the look, so nothing is published unseen in between.
    let mut rx = poller.subscribe();
    loop {
        if let Some(s) = poller.snapshot(id)
            && ok(&s)
        {
            return s;
        }
        match tokio::time::timeout(LONG, rx.recv()).await {
            Ok(Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            other => panic!("no matching snapshot of service {id}: {other:?}"),
        }
    }
}

/// Waits for `Change::Removed(id)` on `rx`.
async fn wait_removed(rx: &mut tokio::sync::broadcast::Receiver<Change>, id: i64) {
    loop {
        match tokio::time::timeout(LONG, rx.recv()).await {
            Ok(Ok(Change::Removed(got))) if got == id => return,
            Ok(Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            other => panic!("service {id} was not announced as removed: {other:?}"),
        }
    }
}

/// Lets the other tasks run (without moving the paused clock) until `done`:
/// for a state that is reached without any announcement.
async fn until_state(what: &str, done: impl Fn() -> bool) {
    for _ in 0..5000 {
        if done() {
            return;
        }
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("gave up waiting for {what}");
}

fn secs(d: Instant, earlier: Instant) -> f64 {
    d.duration_since(earlier).as_secs_f64()
}

/// `gap` is within the jitter (±10 %) of `nominal`.
fn assert_gap(what: &str, gap: f64, nominal: f64) {
    assert!(
        gap >= nominal * 0.9 - 0.001 && gap <= nominal * 1.1 + 0.001,
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

async fn store_snapshot(app: &App, id: i64, snapshot: &Snapshot) {
    let json = serde_json::to_string(snapshot).unwrap();
    let at = snapshot.fetched_at.to_string();
    app.magpie
        .core
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO service_snapshots (service_id, json, fetched_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, json, at],
            )
        })
        .await
        .unwrap();
}

fn down_need(name: &str) -> Need {
    Need {
        key: DOWN_KEY.into(),
        level: Level::Bad,
        label_key: "need.http_down".into(),
        params: [("name".to_string(), name.to_string())].into(),
        score: 1.0,
    }
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
    wait_calls("t_interval", 3).await;
    let c = calls("t_interval");
    assert!(
        secs(c[0].at, started) < 5.0,
        "the first fetch is staggered over the first 5 s, not later"
    );
    assert_gap("1st to 2nd", secs(c[1].at, c[0].at), 60.0);
    assert_gap("2nd to 3rd", secs(c[2].at, c[1].at), 60.0);
    let s = wait_snapshot(&poller, id, |s| s.values.get("n") == Some(&Value::num(3.0))).await;
    assert_eq!(s.status, Status::Ok);
}

#[tokio::test(flavor = "current_thread")]
async fn the_first_fetches_are_spread_over_the_start_up_window() {
    let app = App::new().await;
    tokio::time::pause();
    let tags: Vec<String> = (0..8).map(|i| format!("t_spread_{i}")).collect();
    for tag in &tags {
        add_stub(&app, tag, tag, "", 600).await;
    }
    let started = Instant::now();
    let _poller = start_with(&app, stub_kinds, 5);
    let mut first = Vec::new();
    for tag in &tags {
        wait_calls(tag, 1).await;
        first.push(secs(calls(tag)[0].at, started));
    }
    let (min, max) = first
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), t| (lo.min(*t), hi.max(*t)));
    assert!(max < 5.0, "all within the window: {first:?}");
    assert!(
        max - min > 0.5,
        "spread out, not all at once (eight random starts): {first:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_interval_below_the_kinds_minimum_is_raised_to_it() {
    let app = App::new().await;
    tokio::time::pause();
    // The row says 1 s; the kind's minimum is 10 s.
    add_service(
        &app,
        "stub10",
        "Eager",
        None,
        json!({ "tag": "t_clamp" }),
        1,
    )
    .await;
    let _poller = start_with(&app, stub_kinds, 0);
    wait_calls("t_clamp", 3).await;
    let c = calls("t_clamp");
    assert_gap("1st to 2nd", secs(c[1].at, c[0].at), 10.0);
    assert_gap("2nd to 3rd", secs(c[2].at, c[1].at), 10.0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_failing_fetch_backs_off_twofold_to_ten_minutes_and_resets_on_success() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_backoff", Mode::Err);
    let id = add_stub(&app, "Backoff", "t_backoff", "", 150).await;
    let poller = start_with(&app, stub_kinds, 0);
    wait_calls("t_backoff", 1).await;
    // The failure is a bad reading with a short reason, not a missing one.
    let s = wait_snapshot(&poller, id, |_| true).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("settings not valid"));
    wait_calls("t_backoff", 4).await;
    // Read as the call is made: fetch 5 is the first that succeeds.
    set_mode("t_backoff", Mode::Ok);
    wait_calls("t_backoff", 6).await;
    let c = calls("t_backoff");
    // 2 x 150, 4 x 150, then 8 x 150 is capped at 10 minutes.
    assert_gap("after one failure", secs(c[1].at, c[0].at), 300.0);
    assert_gap("after two failures", secs(c[2].at, c[1].at), 600.0);
    assert_gap("capped", secs(c[3].at, c[2].at), 600.0);
    // Fetch 4 failed too, so fetch 5 is still late; fetch 5 succeeded, so the
    // next one is on time again.
    assert_gap("the last backoff", secs(c[4].at, c[3].at), 600.0);
    assert_gap("reset", secs(c[5].at, c[4].at), 150.0);
}

#[tokio::test(flavor = "current_thread")]
async fn one_hanging_service_does_not_delay_another() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_hang", Mode::Hang);
    let hung = add_stub(&app, "Hangs", "t_hang", "", 10).await;
    let fine = add_stub(&app, "Fine", "t_fine", "", 10).await;
    let started = Instant::now();
    let poller = start_with(&app, stub_kinds, 0);
    wait_calls("t_fine", 3).await;
    assert!(
        secs(Instant::now(), started) < 25.0,
        "the fine one runs on its own schedule"
    );
    assert_eq!(calls("t_hang").len(), 1, "the hanging fetch is still out");
    assert!(poller.snapshot(hung).is_none());
    assert!(poller.snapshot(fine).is_some());
    // A fetch that never ends is given up after a deadline and recorded.
    let s = wait_snapshot(&poller, hung, |_| true).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("timeout"));
    let waited = secs(Instant::now(), started);
    let deadline = FETCH_DEADLINE.as_secs_f64();
    assert!(waited >= deadline && waited < deadline + 1.0, "{waited}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_panicking_kind_gives_a_bad_snapshot_and_is_fetched_again() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_panic", Mode::Panic);
    let id = add_stub(&app, "Panics", "t_panic", "", 10).await;
    let poller = start_with(&app, stub_kinds, 0);
    wait_calls("t_panic", 1).await;
    let s = wait_snapshot(&poller, id, |_| true).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("request failed"));
    set_mode("t_panic", Mode::Ok);
    wait_calls("t_panic", 2).await;
    let s = wait_snapshot(&poller, id, |s| s.status == Status::Ok).await;
    assert_eq!(s.values.get("n"), Some(&Value::num(2.0)));
}

// ---------------------------------------------------------------------------
// Storing and announcing

#[tokio::test(flavor = "current_thread")]
async fn a_snapshot_is_stored_and_loaded_on_restart_before_the_first_fetch() {
    let app = App::new().await;
    tokio::time::pause();
    let id = add_stub(&app, "Stored", "t_stored", "kept", 60).await;
    let poller = start_with(&app, stub_kinds, 0);
    wait_calls("t_stored", 1).await;
    let first = wait_snapshot(&poller, id, |_| true).await;
    // One row, holding the reading.
    let stored = stored_json(&app, id).await.expect("a stored snapshot");
    assert!(stored.contains("kept"), "{stored}");
    assert_eq!(app.count("SELECT COUNT(*) FROM service_snapshots").await, 1);
    drop(poller);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }

    // A new poller, whose first fetch is a long way off.
    let poller = start_with(&app, stub_kinds, 1000);
    until_state("the stored snapshot to load", || {
        poller.snapshot(id).is_some()
    })
    .await;
    let loaded = poller.snapshot(id).unwrap();
    assert_eq!(loaded.fetched_at, first.fetched_at, "its own time, not now");
    assert_eq!(*loaded, *first);
    assert_eq!(calls("t_stored").len(), 1, "nothing has been fetched yet");
    // And then it is replaced by a fresh one.
    wait_calls("t_stored", 2).await;
    let fresh = wait_snapshot(&poller, id, |s| s.values.get("n") == Some(&Value::num(2.0))).await;
    assert_eq!(fresh.status, Status::Ok);
    assert_eq!(app.count("SELECT COUNT(*) FROM service_snapshots").await, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn an_older_stored_shape_loads_and_a_corrupt_row_is_skipped() {
    let app = App::new().await;
    tokio::time::pause();
    let old = add_stub(&app, "Old", "t_old", "", 600).await;
    let corrupt = add_stub(&app, "Corrupt", "t_corrupt", "", 600).await;
    let current = add_stub(&app, "Current", "t_current", "", 600).await;
    // What an earlier version may have written: no values, needs or error.
    app.exec(format!(
        "INSERT INTO service_snapshots (service_id, json, fetched_at)
         VALUES ({old}, '{{\"status\":\"warn\",\"fetched_at\":\"2026-01-02T03:04:05Z\"}}', '2026-01-02T03:04:05Z'),
                ({corrupt}, '{{not json at all', '2026-01-02T03:04:05Z')"
    ))
    .await;
    let mut now = Snapshot::new(Status::Ok);
    now.values.insert("n".into(), Value::num(5.0));
    store_snapshot(&app, current, &now).await;

    let poller = start_with(&app, stub_kinds, 1000);
    until_state("the stored snapshots to load", || {
        poller.snapshot(current).is_some()
    })
    .await;
    let s = poller.snapshot(old).expect("the older shape loads");
    assert_eq!(s.status, Status::Warn);
    assert!(s.values.is_empty() && s.needs.is_empty() && s.error.is_none());
    assert_eq!(
        s.fetched_at,
        "2026-01-02T03:04:05Z".parse::<Timestamp>().unwrap()
    );
    assert!(
        poller.snapshot(corrupt).is_none(),
        "the corrupt row is skipped"
    );
    assert_eq!(*poller.snapshot(current).unwrap(), now);
    // The corrupt one is replaced by its first fetch.
    wait_calls("t_corrupt", 1).await;
    wait_snapshot(&poller, corrupt, |s| s.status == Status::Ok).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_broadcast_goes_out_for_every_new_snapshot() {
    let app = App::new().await;
    tokio::time::pause();
    let a = add_stub(&app, "A", "t_bc_a", "", 10).await;
    let b = add_stub(&app, "B", "t_bc_b", "", 10).await;
    let poller = start_with(&app, stub_kinds, 0);
    let mut rx = poller.subscribe();
    let third = |s: &Snapshot| s.values.get("n") == Some(&Value::num(3.0));
    wait_snapshot(&poller, a, third).await;
    wait_snapshot(&poller, b, third).await;
    let (mut for_a, mut for_b) = (0, 0);
    while let Ok(change) = rx.try_recv() {
        match change {
            Change::Snapshot(id) if id == a => for_a += 1,
            Change::Snapshot(id) if id == b => for_b += 1,
            other => panic!("a broadcast {other:?}"),
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
    wait_calls("t_update", 1).await;
    wait_snapshot(&poller, id, |_| true).await;

    app.exec(format!(
        "UPDATE services SET settings = '{}' WHERE id = {id}",
        json!({ "tag": "t_update", "marker": "new" })
    ))
    .await;
    app.magpie
        .service_events
        .send(ServiceEvent::Updated(id))
        .unwrap();
    // The fetch is at once, not at the next interval an hour away.
    let s = wait_snapshot(&poller, id, |s| {
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
    let id = add_stub(&app, "Added", "t_created", "", 60).await;
    let started = Instant::now();
    app.magpie
        .service_events
        .send(ServiceEvent::Created(id))
        .unwrap();
    wait_snapshot(&poller, id, |_| true).await;
    assert_eq!(calls("t_created").len(), 1);
    assert!(secs(Instant::now(), started) < 1.0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_deleted_service_stops_and_its_row_is_gone() {
    let app = App::new().await;
    tokio::time::pause();
    let id = add_stub(&app, "Doomed", "t_delete", "", 10).await;
    let other = add_stub(&app, "Stays", "t_stays", "", 10).await;
    let poller = start_with(&app, stub_kinds, 0);
    wait_snapshot(&poller, id, |_| true).await;
    wait_snapshot(&poller, other, |_| true).await;
    assert!(stored_json(&app, id).await.is_some());

    let mut rx = poller.subscribe();
    assert!(store::delete(&app.magpie.core, id).await.unwrap());
    app.magpie
        .service_events
        .send(ServiceEvent::Deleted(id))
        .unwrap();
    wait_removed(&mut rx, id).await;
    assert!(poller.snapshot(id).is_none());
    assert!(stored_json(&app, id).await.is_none());
    let before = calls("t_delete").len();
    wait_calls("t_stays", 5).await;
    assert_eq!(calls("t_delete").len(), before, "the task has stopped");
    // Subscribers (the live stream) were told once that the service is gone.
    let mut removed = 0;
    while let Ok(change) = rx.try_recv() {
        match change {
            Change::Removed(got) if got == id => removed += 1,
            Change::Snapshot(got) => assert_ne!(got, id, "no snapshot after the removal"),
            Change::Removed(_) => {}
        }
    }
    assert_eq!(removed, 0, "announced once, and wait_removed took it");
    assert!(poller.snapshot(id).is_none());
    assert!(poller.snapshot(other).is_some());
    assert_eq!(app.count("SELECT COUNT(*) FROM service_snapshots").await, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn a_fetch_that_is_out_when_the_service_is_deleted_leaves_nothing_behind() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_late", Mode::Block);
    let id = add_stub(&app, "Late", "t_late", "", 10).await;
    let poller = start_with(&app, stub_kinds, 0);
    let mut rx = poller.subscribe();
    wait_calls("t_late", 1).await;

    // The fetch is out, waiting at its gate. The service is deleted.
    assert!(store::delete(&app.magpie.core, id).await.unwrap());
    app.magpie
        .service_events
        .send(ServiceEvent::Deleted(id))
        .unwrap();
    wait_removed(&mut rx, id).await;
    // Now the fetch is let go, and a long time passes.
    set_mode("t_late", Mode::Ok);
    gate("t_late").notify_waiters();
    gate("t_late").notify_one();
    tokio::time::sleep(Duration::from_secs(300)).await;

    assert_eq!(calls("t_late").len(), 1, "no second fetch");
    assert!(poller.snapshot(id).is_none());
    assert!(stored_json(&app, id).await.is_none(), "no row");
    assert_eq!(app.count("SELECT COUNT(*) FROM service_snapshots").await, 0);
    let mut seen = Vec::new();
    while let Ok(change) = rx.try_recv() {
        seen.push(change);
    }
    assert!(
        !seen.contains(&Change::Snapshot(id)),
        "no snapshot announced after the removal: {seen:?}"
    );
    assert!(
        !seen.contains(&Change::Removed(id)),
        "exactly one removal notice (taken above): {seen:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reload_and_remove_work_without_an_event() {
    let app = App::new().await;
    tokio::time::pause();
    let poller = start_with(&app, stub_kinds, 1000);
    let id = add_stub(&app, "Handle", "t_handle", "", 60).await;
    poller.reload(id);
    wait_snapshot(&poller, id, |_| true).await;
    let mut rx = poller.subscribe();
    poller.remove(id);
    assert!(
        poller.snapshot(id).is_none(),
        "gone as soon as remove returns"
    );
    wait_removed(&mut rx, id).await;
    assert!(stored_json(&app, id).await.is_none());
    assert!(rx.try_recv().is_err(), "announced once");
}

#[tokio::test(flavor = "current_thread")]
async fn an_idle_handle_holds_set_snapshots_and_announces_removal() {
    let poller = PollerHandle::idle();
    let mut rx = poller.subscribe();
    poller.set_snapshot_for_tests(7, Snapshot::new(Status::Warn));
    assert_eq!(poller.snapshot(7).unwrap().status, Status::Warn);
    assert_eq!(rx.try_recv(), Ok(Change::Snapshot(7)));
    poller.remove(7);
    assert!(poller.snapshot(7).is_none());
    assert_eq!(rx.try_recv(), Ok(Change::Removed(7)));
}

#[tokio::test(flavor = "current_thread")]
async fn after_missed_events_only_new_or_changed_services_are_restarted() {
    let app = App::new().await;
    tokio::time::pause();
    let a = add_stub(&app, "Unchanged", "t_rs_a", "", 600).await;
    let b = add_stub(&app, "Changed", "t_rs_b", "", 600).await;
    let poller = start_with(&app, stub_kinds, 5);
    wait_calls("t_rs_a", 1).await;
    wait_calls("t_rs_b", 1).await;
    wait_snapshot(&poller, a, |_| true).await;
    wait_snapshot(&poller, b, |_| true).await;

    // B's interval changes and C is new, and the events about them are lost:
    // more than the channel holds go by before the poller looks.
    app.exec(format!(
        "UPDATE services SET interval_s = 300 WHERE id = {b}"
    ))
    .await;
    let c = add_stub(&app, "New", "t_rs_c", "", 600).await;
    let flooded = Instant::now();
    for i in 0..200 {
        let _ = app
            .magpie
            .service_events
            .send(ServiceEvent::Updated(900_000 + i));
    }
    wait_calls("t_rs_b", 2).await;
    wait_calls("t_rs_c", 1).await;
    // Past the start-up window: the unchanged one was left alone.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(calls("t_rs_a").len(), 1, "unchanged: not restarted");
    assert_eq!(calls("t_rs_b").len(), 2);
    assert_eq!(calls("t_rs_c").len(), 1);
    for tag in ["t_rs_b", "t_rs_c"] {
        let last = calls(tag).last().unwrap().at;
        assert!(
            secs(last, flooded) < 5.0,
            "{tag} is staggered over the window, not later"
        );
    }
    assert!(poller.snapshot(c).is_some());
}

// ---------------------------------------------------------------------------
// «Trenger deg» after two bad fetches in a row

#[tokio::test(flavor = "current_thread")]
async fn two_bad_fetches_in_a_row_make_a_need_and_a_good_one_clears_it() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_need", Mode::Bad);
    let id = add_service(&app, "http", "Needy", None, json!({ "tag": "t_need" }), 10).await;
    let poller = start_with(&app, stub_as_http_kinds, 0);
    wait_calls("t_need", 1).await;
    let first = wait_snapshot(&poller, id, |_| true).await;
    assert_eq!(first.status, Status::Bad);
    assert!(first.needs.is_empty(), "one bad fetch is not yet a need");
    wait_calls("t_need", 2).await;
    let second = wait_snapshot(&poller, id, |s| !s.needs.is_empty()).await;
    assert_eq!(second.needs.len(), 1);
    let need = &second.needs[0];
    assert_eq!(need.key, DOWN_KEY);
    assert_eq!(need.label_key, "need.http_down");
    assert_eq!(need.level, Level::Bad);
    assert_eq!(need.params.get("name").map(String::as_str), Some("Needy"));

    set_mode("t_need", Mode::Ok);
    wait_calls("t_need", 3).await;
    let third = wait_snapshot(&poller, id, |s| s.status == Status::Ok).await;
    assert!(third.needs.is_empty());

    // Bad, good, bad again: not two in a row.
    set_mode("t_need", Mode::Bad);
    wait_calls("t_need", 4).await;
    let fourth = wait_snapshot(&poller, id, |s| s.status == Status::Bad).await;
    assert!(fourth.needs.is_empty(), "the streak started over");
}

#[tokio::test(flavor = "current_thread")]
async fn an_error_counts_as_a_bad_fetch_for_the_need() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_need_err", Mode::ErrConnect);
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
    let s = wait_snapshot(&poller, id, |s| !s.needs.is_empty()).await;
    assert_eq!(s.needs[0].key, DOWN_KEY);
    assert_eq!(calls("t_need_err").len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn unusable_settings_or_secret_need_an_admin_not_the_site() {
    let app = App::new().await;
    tokio::time::pause();
    // The kind has no "down" rule at all; the config need is the engine's.
    for (tag, mode) in [
        ("t_cfg", Mode::Err),
        ("t_cfg_secret", Mode::ErrSecret),
        ("t_cfg_auth", Mode::ErrAuth),
    ] {
        set_mode(tag, mode);
        let id = add_stub(&app, tag, tag, "", 10).await;
        let poller = start_with(&app, stub_kinds, 0);
        let s = wait_snapshot(&poller, id, |s| !s.needs.is_empty()).await;
        assert_eq!(
            calls(tag).len(),
            2,
            "after the second bad reading, not the first"
        );
        assert_eq!(s.needs.len(), 1);
        assert_eq!(s.needs[0].key, CONFIG_KEY);
        assert_eq!(s.needs[0].label_key, "need.service_config");
        assert_eq!(s.needs[0].params.get("name").map(String::as_str), Some(tag));
        drop(poller);
    }
    // For the http kind too: not "down".
    set_mode("t_cfg_http", Mode::Err);
    let id = add_service(
        &app,
        "http",
        "Misset",
        None,
        json!({ "tag": "t_cfg_http" }),
        10,
    )
    .await;
    let poller = start_with(&app, stub_as_http_kinds, 0);
    let s = wait_snapshot(&poller, id, |s| !s.needs.is_empty()).await;
    assert_eq!(s.needs[0].key, CONFIG_KEY);
}

#[tokio::test(flavor = "current_thread")]
async fn a_kind_without_the_rule_gets_no_down_need() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_no_rule", Mode::Bad);
    let id = add_stub(&app, "NoRule", "t_no_rule", "", 10).await;
    let poller = start_with(&app, stub_kinds, 0);
    wait_calls("t_no_rule", 3).await;
    let s = wait_snapshot(&poller, id, |_| true).await;
    assert!(s.needs.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_need_that_is_showing_survives_a_restart() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_keep", Mode::Bad);
    let id = add_service(&app, "http", "Keeper", None, json!({ "tag": "t_keep" }), 10).await;
    // What a poller left behind: bad, with the need showing.
    let mut stored = Snapshot::failed(&FetchError::Connect);
    stored.fetched_at = Timestamp::from_second(1_700_000_000).unwrap();
    stored.needs.push(down_need("Keeper"));
    store_snapshot(&app, id, &stored).await;

    let poller = start_with(&app, stub_as_http_kinds, 0);
    // One bad fetch after the restart: the need is still there, it did not
    // go and come back (an acknowledgement would reopen).
    let s = wait_snapshot(&poller, id, |s| s.fetched_at > stored.fetched_at).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.needs.len(), 1, "{s:?}");
    assert_eq!(s.needs[0].key, DOWN_KEY);
    assert_eq!(calls("t_keep").len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn a_bad_snapshot_without_a_need_counts_as_the_first_bad_reading() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_seed1", Mode::Bad);
    let id = add_service(&app, "http", "Half", None, json!({ "tag": "t_seed1" }), 10).await;
    let mut stored = Snapshot::failed(&FetchError::Connect);
    stored.fetched_at = Timestamp::from_second(1_700_000_000).unwrap();
    store_snapshot(&app, id, &stored).await;
    let poller = start_with(&app, stub_as_http_kinds, 0);
    // Bad before the restart, bad after it: two in a row.
    let s = wait_snapshot(&poller, id, |s| s.fetched_at > stored.fetched_at).await;
    assert_eq!(s.needs.len(), 1, "{s:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_need_that_is_showing_survives_an_update_of_an_unrelated_field() {
    let app = App::new().await;
    tokio::time::pause();
    set_mode("t_keep_upd", Mode::Bad);
    let id = add_service(
        &app,
        "http",
        "Before",
        None,
        json!({ "tag": "t_keep_upd" }),
        10,
    )
    .await;
    let poller = start_with(&app, stub_as_http_kinds, 0);
    wait_calls("t_keep_upd", 2).await;
    let shown = wait_snapshot(&poller, id, |s| !s.needs.is_empty()).await;

    // Only the name changes.
    app.exec(format!(
        "UPDATE services SET name = 'After' WHERE id = {id}"
    ))
    .await;
    app.magpie
        .service_events
        .send(ServiceEvent::Updated(id))
        .unwrap();
    let s = wait_snapshot(&poller, id, |s| s.fetched_at > shown.fetched_at).await;
    assert_eq!(s.needs.len(), 1, "the need did not go: {s:?}");
    assert_eq!(s.needs[0].key, DOWN_KEY);
    assert_eq!(
        s.needs[0].params.get("name").map(String::as_str),
        Some("After")
    );
    assert_eq!(calls("t_keep_upd").len(), 3);
}

// ---------------------------------------------------------------------------
// Ids are not used again

#[tokio::test(flavor = "current_thread")]
async fn a_service_id_is_never_used_again() {
    let app = App::new().await;
    let first = add_stub(&app, "First", "t_ids", "", 60).await;
    let second = add_stub(&app, "Second", "t_ids", "", 60).await;
    assert!(store::delete(&app.magpie.core, second).await.unwrap());
    let third = add_stub(&app, "Third", "t_ids", "", 60).await;
    assert!(first < second && second < third, "{first} {second} {third}");
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
    // Nothing panicked in the poller or in a fetch of these services (the
    // panicking stub of another test logs "a fetch panicked" in the same
    // buffer, so the exact lines are looked for).
    let log = logged();
    assert!(!log.contains("a poller task panicked"));
    assert!(
        !log.lines()
            .any(|l| l.contains("a fetch panicked") && l.contains("kind=http")),
        "an http fetch panicked"
    );
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
    // The capture works: the poller does say what it is doing, and the
    // libraries' lines (hyper, reqwest) are in it.
    assert!(logged().contains("poller"), "{}", logged());
    assert!(
        logged().contains("hyper_util") || logged().contains("reqwest"),
        "no library line was captured:
{}",
        logged()
    );
}

#[tokio::test]
async fn a_secret_that_cannot_be_opened_is_recorded_as_such_not_as_bad_settings() {
    let app = App::new().await;
    let base = stub_server().await;
    let id = store::create(
        &app.magpie.core,
        &ServiceInput {
            kind: "http".into(),
            name: "Locked".into(),
            url: Some(format!("{base}/ok")),
            settings: json!({}),
            secret: SecretChange::Replace(SECRET.into()),
            interval_s: 60,
            visibility: Visibility::Admin,
            users: vec![],
        },
    )
    .await
    .unwrap();
    // The instance key changed: what is stored no longer opens.
    app.exec(format!(
        "UPDATE services SET secret = 'sealed:v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA' WHERE id = {id}"
    ))
    .await;
    let poller = start_with(&app, stub_kinds, 0);
    let s = real_snapshot(&poller, id).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(
        s.error.as_deref(),
        Some(FetchError::SecretUnreadable.to_string().as_str())
    );
    assert_ne!(s.error, Some(FetchError::Config.to_string()));
    assert_eq!(
        integrations::error_text_key(s.error.as_deref().unwrap()),
        Some("fetch_err.secret")
    );
}

/// A server that accepts connections and never answers.
async fn silent_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            if let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_site_that_never_answers_is_bad_with_a_timeout_and_the_others_go_on() {
    let app = App::new().await;
    let silent = silent_server().await;
    let hung = add_service(
        &app,
        "http",
        "Silent",
        Some(format!("{silent}/")),
        json!({}),
        60,
    )
    .await;
    add_stub(&app, "Other", "t_real_other", "", 1).await;
    let started = std::time::Instant::now();
    let poller = start_with(&app, stub_kinds, 0);
    let s = real_snapshot(&poller, hung).await;
    let took = started.elapsed();
    assert_eq!(s.status, Status::Bad, "{s:?}");
    assert_eq!(s.error.as_deref(), Some("timeout"));
    assert!(
        took >= Duration::from_secs(9) && took < Duration::from_secs(14),
        "the client's own 10 s limit, not the poller's 30 s: {took:?}"
    );
    assert!(
        calls("t_real_other").len() >= 5,
        "the other service kept updating meanwhile: {}",
        calls("t_real_other").len()
    );
}
