//! Magpie's own history of every number (`src/history.rs`): what the poller
//! records, the bounds of the two tables, and that they go with their service.
//!
//! What reaches a page (embed, stream, token API) is in `tests/values.rs`.

mod common;

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use common::App;
use mindful_magpie::history::{self, History, MAX_POINTS, Write};
use mindful_magpie::integrations::poller::{Change, Options, Poller};
use mindful_magpie::integrations::{
    self, BoxFuture, FetchCtx, FetchError, Field, Format, Integration, MetricDef, Snapshot, Status,
    Value,
};
use mindful_magpie::services::{SecretChange, ServiceEvent, ServiceInput, Visibility, store};
use serde_json::json;

/// A Monday, 00:00 UTC, a multiple of 300 s.
const T0: i64 = 1_762_128_000;
const DAY: i64 = 86_400;

async fn service(app: &App, name: &str) -> i64 {
    store::create(
        &app.magpie.core,
        &ServiceInput {
            kind: "http".into(),
            name: name.into(),
            url: Some("http://10.55.66.77:8123/".into()),
            settings: json!({}),
            secret: SecretChange::Keep,
            interval_s: 60,
            visibility: Visibility::All,
            users: vec![],
        },
    )
    .await
    .unwrap()
}

fn one(metric: &str, v: f64) -> Vec<(String, f64)> {
    vec![(metric.to_string(), v)]
}

/// Records and stores one reading, as the poller does.
async fn feed(
    app: &App,
    h: &History,
    id: i64,
    interval_s: i64,
    at: i64,
    readings: Vec<(String, f64)>,
) -> Write {
    let w = h.record(id, interval_s, at, readings);
    history::persist(&app.magpie.core, w.clone()).await.unwrap();
    w
}

async fn rows(app: &App, id: i64, metric: &str) -> Vec<(i64, f64)> {
    let metric = metric.to_string();
    app.magpie
        .core
        .with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT at, value FROM service_history WHERE service_id = ?1 AND metric = ?2 ORDER BY at",
            )?;
            stmt.query_map(rusqlite::params![id, metric], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()
        })
        .await
        .unwrap()
}

async fn daily_rows(app: &App, id: i64, metric: &str) -> Vec<(String, f64, f64, f64, i64)> {
    let metric = metric.to_string();
    app.magpie
        .core
        .with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT day, min, max, avg, n FROM service_history_daily
                 WHERE service_id = ?1 AND metric = ?2 ORDER BY day",
            )?;
            stmt.query_map(rusqlite::params![id, metric], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .await
        .unwrap()
}

fn set(metrics: &[&str]) -> BTreeSet<String> {
    metrics.iter().map(|m| (*m).to_string()).collect()
}

// ---------------------------------------------------------------------------
// Peaks

#[tokio::test]
async fn a_peak_metric_keeps_the_highest_reading_of_a_slot_in_memory_and_in_the_table() {
    let app = App::new().await;
    let id = service(&app, "Monitor").await;
    let h = History::default();
    let peaks = set(&["down"]);
    // A monitor is down at 00:00:10 and back at 00:01:10, in the same slot.
    for (at, down, latency) in [(T0 + 10, 1.0, 5.0), (T0 + 70, 0.0, 6.0)] {
        let w = h.record_with_peaks(
            id,
            60,
            at,
            vec![("down".into(), down), ("latency_ms".into(), latency)],
            &peaks,
        );
        history::persist(&app.magpie.core, w).await.unwrap();
    }
    let held = h.series(id, &set(&["down", "latency_ms"]), T0 + 70);
    assert_eq!(held["down"].len(), 1);
    assert_eq!((held["down"][0].at, held["down"][0].value), (T0, 1.0));
    // Any other metric: the latest wins, as before.
    assert_eq!(held["latency_ms"][0].value, 6.0);
    assert_eq!(rows(&app, id, "down").await, vec![(T0, 1.0)]);
    assert_eq!(rows(&app, id, "latency_ms").await, vec![(T0, 6.0)]);

    // The table keeps the peak also when the memory has not seen it (a
    // restart in the middle of the slot).
    let fresh = History::default();
    let w = fresh.record_with_peaks(id, 60, T0 + 130, one("down", 0.0), &peaks);
    assert_eq!(w.peaks, set(&["down"]));
    history::persist(&app.magpie.core, w).await.unwrap();
    assert_eq!(rows(&app, id, "down").await, vec![(T0, 1.0)]);

    // The next slot starts again from its own readings.
    let w = h.record_with_peaks(id, 60, T0 + 310, one("down", 0.0), &peaks);
    history::persist(&app.magpie.core, w).await.unwrap();
    assert_eq!(
        rows(&app, id, "down").await,
        vec![(T0, 1.0), (T0 + 300, 0.0)]
    );
}

#[tokio::test]
async fn vv_marks_its_down_count_as_a_peak_and_nothing_else_does() {
    let peaks: Vec<(&str, &str)> = integrations::registry()
        .iter()
        .flat_map(|k| {
            k.metrics()
                .iter()
                .filter(|m| m.peak)
                .map(move |m| (k.kind(), m.id))
        })
        .collect();
    assert_eq!(peaks, [("vv", "down")]);
}

// ---------------------------------------------------------------------------
// Bounds

#[tokio::test]
async fn a_thousand_readings_at_15_seconds_are_thinned_to_one_point_per_5_minutes() {
    let app = App::new().await;
    let id = service(&app, "Blog").await;
    let h = History::default();
    for i in 0..1000 {
        feed(&app, &h, id, 15, T0 + i * 15, one("latency_ms", i as f64)).await;
    }
    // 1000 x 15 s is 15000 s: 50 slots, each holding the last reading in it.
    let stored = rows(&app, id, "latency_ms").await;
    assert_eq!(stored.len(), 50);
    assert!(stored.iter().all(|(at, _)| at % 300 == 0));
    assert_eq!(stored[0], (T0, 19.0));
    assert_eq!(stored[49], (T0 + 49 * 300, 999.0));
    let memory = h.series(id, &set(&["latency_ms"]), T0 + 999 * 15);
    assert_eq!(memory["latency_ms"].len(), 50);
    assert_eq!(memory["latency_ms"][0].value, 19.0);
}

#[tokio::test]
async fn a_long_run_keeps_at_most_288_points_covering_24_hours() {
    let app = App::new().await;
    let id = service(&app, "Blog").await;
    let h = History::default();
    // 7000 readings at 15 s: about 29 hours.
    let readings = 7000;
    for i in 0..readings {
        feed(
            &app,
            &h,
            id,
            15,
            T0 + i * 15,
            one("latency_ms", (i % 97) as f64),
        )
        .await;
    }
    let last = T0 + (readings - 1) * 15;
    let stored = rows(&app, id, "latency_ms").await;
    assert!(stored.len() <= MAX_POINTS, "{} points", stored.len());
    assert!(stored.len() >= MAX_POINTS - 1, "{} points", stored.len());
    assert!(stored.first().unwrap().0 >= last - DAY);
    assert_eq!(stored.last().unwrap().0, last - last % 300);
    let memory = h.series(id, &set(&["latency_ms"]), last);
    assert!(memory["latency_ms"].len() <= MAX_POINTS);
    assert_eq!(memory["latency_ms"].len(), stored.len());
}

#[tokio::test]
async fn a_slow_service_keeps_a_point_per_reading() {
    let app = App::new().await;
    let id = service(&app, "Backup").await;
    let h = History::default();
    // Every 10 minutes for 23 hours: a point per reading, none thinned.
    for i in 0..138 {
        feed(&app, &h, id, 600, T0 + i * 600 + 7, one("size", i as f64)).await;
    }
    let stored = rows(&app, id, "size").await;
    assert_eq!(stored.len(), 138);
    assert_eq!(stored[0].0, T0 + 7, "the time of the reading, not a slot");
}

#[tokio::test]
async fn the_daily_rows_roll_up_every_reading_and_go_after_30_days() {
    let app = App::new().await;
    let id = service(&app, "Blog").await;
    let h = History::default();
    // Three readings the first day, all in one 5-minute slot: the series keeps
    // one point, the day keeps all three.
    for (i, v) in [10.0, 20.0, 60.0].into_iter().enumerate() {
        feed(&app, &h, id, 15, T0 + 3600 + i as i64 * 15, one("m", v)).await;
    }
    assert_eq!(rows(&app, id, "m").await.len(), 1);
    let day0 = daily_rows(&app, id, "m").await;
    assert_eq!(day0.len(), 1);
    assert_eq!(day0[0].0, "2025-11-03");
    assert_eq!(
        (day0[0].1, day0[0].2, day0[0].3, day0[0].4),
        (10.0, 60.0, 30.0, 3)
    );

    // One reading a day for the next 40 days.
    for d in 1..=40 {
        feed(&app, &h, id, 60, T0 + d * DAY + 60, one("m", d as f64)).await;
    }
    let days = daily_rows(&app, id, "m").await;
    assert_eq!(days.len(), 30, "30 days, today included");
    // Reading 40 (day 40) is the newest; day 11 is the oldest kept.
    assert_eq!(days.last().unwrap().1, 40.0);
    assert_eq!(days.first().unwrap().1, 11.0);
    assert!(days.iter().all(|d| d.4 == 1));
    // The 24-hour series has only the last day.
    assert_eq!(rows(&app, id, "m").await.len(), 1);
}

#[tokio::test]
async fn a_metric_the_service_stops_reporting_is_pruned_with_the_others() {
    let app = App::new().await;
    let id = service(&app, "Blog").await;
    let h = History::default();
    feed(
        &app,
        &h,
        id,
        60,
        T0,
        vec![("gone".into(), 1.0), ("kept".into(), 2.0)],
    )
    .await;
    assert_eq!(rows(&app, id, "gone").await.len(), 1);
    // 31 hours later only `kept` is reported: `gone` is deleted by that write.
    feed(&app, &h, id, 60, T0 + 31 * 3600, one("kept", 3.0)).await;
    assert!(rows(&app, id, "gone").await.is_empty());
    assert_eq!(rows(&app, id, "kept").await.len(), 1);
    // 31 days later its daily row goes too.
    feed(&app, &h, id, 60, T0 + 31 * DAY, one("kept", 4.0)).await;
    assert!(daily_rows(&app, id, "gone").await.is_empty());
    assert_eq!(
        app.count("SELECT COUNT(*) FROM service_history_daily WHERE metric = 'kept'")
            .await,
        1
    );
}

#[tokio::test]
async fn a_service_that_is_gone_gets_no_history_from_a_late_reading() {
    let app = App::new().await;
    let id = service(&app, "Doomed").await;
    assert!(store::delete(&app.magpie.core, id).await.unwrap());
    let h = History::default();
    feed(&app, &h, id, 60, T0, one("m", 1.0)).await;
    assert_eq!(app.count("SELECT COUNT(*) FROM service_history").await, 0);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM service_history_daily")
            .await,
        0
    );
}

// ---------------------------------------------------------------------------
// Deletion

#[tokio::test]
async fn deleting_a_service_deletes_its_history_and_only_its_own() {
    let app = App::new().await;
    let doomed = service(&app, "Doomed").await;
    let stays = service(&app, "Stays").await;
    let h = History::default();
    for id in [doomed, stays] {
        feed(&app, &h, id, 60, T0, one("m", 1.0)).await;
        feed(&app, &h, id, 60, T0 + 600, one("m", 2.0)).await;
    }
    assert_eq!(app.count("SELECT COUNT(*) FROM service_history").await, 4);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM service_history_daily")
            .await,
        2
    );

    assert!(store::delete(&app.magpie.core, doomed).await.unwrap());
    assert!(rows(&app, doomed, "m").await.is_empty());
    assert!(daily_rows(&app, doomed, "m").await.is_empty());
    assert_eq!(rows(&app, stays, "m").await.len(), 2);
    assert_eq!(daily_rows(&app, stays, "m").await.len(), 1);
}

// ---------------------------------------------------------------------------
// The poller records

/// A kind that reads a number, a text, `NaN` and infinity, and counts.
struct Reader;

static READER: Reader = Reader;
static CALLS: Mutex<usize> = Mutex::new(0);
static NO_FIELDS: [Field; 0] = [];
/// `ts` is a moment: a timestamp makes no curve.
static METRICS: [MetricDef; 1] = [MetricDef {
    id: "ts",
    label_key: "kind.stub",
    format: Format::Since,
    peak: false,
}];

impl Integration for Reader {
    fn kind(&self) -> &'static str {
        "history_reader"
    }
    fn label_key(&self) -> &'static str {
        "kind.stub"
    }
    fn fields(&self) -> &'static [Field] {
        &NO_FIELDS
    }
    fn metrics(&self) -> &'static [MetricDef] {
        &METRICS
    }
    fn default_interval(&self) -> Duration {
        Duration::from_secs(60)
    }
    fn min_interval(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn fetch<'a>(&'a self, ctx: &'a FetchCtx) -> BoxFuture<'a, Result<Snapshot, FetchError>> {
        Box::pin(async move {
            let n = {
                let mut calls = CALLS.lock().unwrap();
                *calls += 1;
                *calls
            };
            let bad = ctx.settings["bad"].as_bool().unwrap_or(false);
            let mut snapshot = Snapshot::new(if bad { Status::Unknown } else { Status::Ok });
            snapshot.values.insert("n".into(), Value::num(n as f64));
            snapshot.values.insert("label".into(), Value::text("up"));
            snapshot
                .values
                .insert("ts".into(), Value::num(1_762_900_000.0));
            snapshot.values.insert("nan".into(), Value::num(f64::NAN));
            snapshot
                .values
                .insert("inf".into(), Value::num(f64::INFINITY));
            snapshot.values.insert(
                "both".into(),
                Value {
                    num: Some(2.5),
                    text: Some("2.5 s".into()),
                },
            );
            Ok(snapshot)
        })
    }
}

fn kinds(kind: &str) -> Option<&'static dyn Integration> {
    match kind {
        "history_reader" => Some(&READER),
        other => integrations::find(other),
    }
}

async fn reader(app: &App, name: &str, bad: bool) -> i64 {
    let settings = json!({ "bad": bad }).to_string();
    let name = name.to_string();
    app.magpie
        .core
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO services
                   (kind, name, url, settings, secret, interval_s, visibility, created_at, updated_at)
                 VALUES ('history_reader', ?1, NULL, ?2, NULL, 60, 'admin', '2026-11-07T00:00:00Z', '2026-11-07T00:00:00Z')",
                rusqlite::params![name, settings],
            )?;
            Ok(c.last_insert_rowid())
        })
        .await
        .unwrap()
}

async fn start(app: &App) -> mindful_magpie::integrations::poller::PollerHandle {
    Poller::start_with(
        app.magpie.core.clone(),
        integrations::client().clone(),
        &app.magpie.service_events,
        Options {
            kinds,
            spread: Duration::ZERO,
        },
    )
    .await
}

/// Waits (in real time) until `done` holds.
async fn eventually(what: &str, done: impl AsyncFn() -> bool) {
    for _ in 0..400 {
        if done().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("gave up waiting for {what}");
}

#[tokio::test]
async fn the_poller_records_numbers_only_and_not_for_a_reading_that_is_not_good() {
    let app = App::new().await;
    let good = reader(&app, "Good", false).await;
    let unknown = reader(&app, "Unknown", true).await;
    let poller = start(&app).await;
    eventually("the history of the good service", async || {
        !rows(&app, good, "n").await.is_empty()
    })
    .await;
    // The unknown one was read too (it has a snapshot), and left nothing.
    eventually("a snapshot of the other", async || {
        poller.snapshot(unknown).is_some()
    })
    .await;

    // Numbers are there, rounded; text, NaN and infinity are not.
    assert_eq!(rows(&app, good, "both").await.len(), 1);
    assert_eq!(rows(&app, good, "both").await[0].1, 2.5);
    for skipped in ["label", "nan", "inf", "ts"] {
        assert!(rows(&app, good, skipped).await.is_empty(), "{skipped}");
        assert!(
            daily_rows(&app, good, skipped).await.is_empty(),
            "{skipped}"
        );
    }
    assert_eq!(
        app.count("SELECT COUNT(*) FROM service_history WHERE service_id IN (SELECT id FROM services WHERE name = 'Unknown')").await,
        0
    );
    // The memory has the same, for the page.
    let now = jiff::Timestamp::now().as_second();
    let series = poller.history(good, &set(&["n", "label", "nan", "both"]), now);
    assert_eq!(
        series.keys().cloned().collect::<Vec<_>>(),
        vec!["both", "n"]
    );
    assert!(
        poller
            .history(unknown, &set(&["n", "both"]), now)
            .is_empty()
    );
    // The daily row has the reading.
    assert!(daily_rows(&app, good, "both").await[0].4 >= 1);
}

#[tokio::test]
async fn a_restart_loads_the_last_24_hours_back() {
    let app = App::new().await;
    let id = service(&app, "Blog").await;
    let now = jiff::Timestamp::now().as_second();
    // One point 2 hours old, and one 30 hours old that a late write leaves in
    // the table (a second `History`, so the memory does not drop it first):
    // the load reads the window only.
    feed(
        &app,
        &History::default(),
        id,
        600,
        now - 2 * 3600,
        one("m", 2.0),
    )
    .await;
    feed(
        &app,
        &History::default(),
        id,
        600,
        now - 30 * 3600,
        one("m", 1.0),
    )
    .await;
    assert_eq!(rows(&app, id, "m").await.len(), 2);
    let app = app.restart().await;
    // No kind: the poller loads and then has nothing to fetch.
    let poller = Poller::start_with(
        app.magpie.core.clone(),
        integrations::client().clone(),
        &app.magpie.service_events,
        Options {
            kinds: |_| None,
            spread: Duration::ZERO,
        },
    )
    .await;
    let series = poller.history(id, &set(&["m"]), now);
    assert_eq!(series["m"].len(), 1, "{series:?}");
    assert_eq!(series["m"][0].value, 2.0);
}

#[tokio::test]
async fn deleting_a_service_through_the_poller_frees_its_history_in_memory() {
    let app = App::new().await;
    let id = reader(&app, "Doomed", false).await;
    let poller = start(&app).await;
    let now = jiff::Timestamp::now().as_second();
    eventually("a point in memory", async || {
        !poller.history(id, &set(&["n"]), now).is_empty()
    })
    .await;
    eventually("the point in the table", async || {
        !rows(&app, id, "n").await.is_empty()
    })
    .await;

    let mut rx = poller.subscribe();
    assert!(store::delete(&app.magpie.core, id).await.unwrap());
    app.magpie
        .service_events
        .send(ServiceEvent::Deleted(id))
        .unwrap();
    loop {
        match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
            Ok(Ok(Change::Removed(got))) if got == id => break,
            Ok(_) => {}
            Err(_) => panic!("the service was not announced as removed"),
        }
    }
    assert!(poller.history(id, &set(&["n", "both"]), now).is_empty());
    assert!(rows(&app, id, "n").await.is_empty());
    assert!(daily_rows(&app, id, "n").await.is_empty());
}
