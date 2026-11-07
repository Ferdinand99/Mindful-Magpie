//! «Not answering» for the app kinds: a service whose fetch fails the way an
//! unreachable one does gets the generic down need once that has happened
//! twice in a row, a failure only an admin can fix gets the «set it up again»
//! need and never this one, and a single failed call raises nothing.
//!
//! The real kinds run under the real poller against a stub web server, so the
//! clock is the real one; the kinds are wrapped only to give them a one-second
//! interval.

mod common;

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::App;
use common::stub::{self, Answer};
use mindful_magpie::integrations::poller::{CONFIG_KEY, DOWN_KEY, Options, Poller, PollerHandle};
use mindful_magpie::integrations::{
    self, BoxFuture, FetchCtx, FetchError, Field, Integration, MetricDef, SettingError, Snapshot,
    Status,
};
use mindful_magpie::services::{SecretChange, ServiceInput, Visibility, store};
use serde_json::{Value as Json, json};

/// A real kind with a shorter minimum interval, so a test need not wait 15 s.
struct Quick(&'static dyn Integration);

impl Integration for Quick {
    fn kind(&self) -> &'static str {
        self.0.kind()
    }

    fn label_key(&self) -> &'static str {
        self.0.label_key()
    }

    fn fields(&self) -> &'static [Field] {
        self.0.fields()
    }

    fn metrics(&self) -> &'static [MetricDef] {
        self.0.metrics()
    }

    fn default_interval(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn min_interval(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn url_required(&self) -> bool {
        self.0.url_required()
    }

    fn check_settings(&self, settings: &serde_json::Map<String, Json>) -> Result<(), SettingError> {
        self.0.check_settings(settings)
    }

    fn fetch<'a>(&'a self, ctx: &'a FetchCtx) -> BoxFuture<'a, Result<Snapshot, FetchError>> {
        self.0.fetch(ctx)
    }
}

fn kinds(name: &str) -> Option<&'static dyn Integration> {
    static VV: OnceLock<Quick> = OnceLock::new();
    static TOFA: OnceLock<Quick> = OnceLock::new();
    match name {
        "vv" => Some(VV.get_or_init(|| Quick(integrations::find("vv").unwrap()))),
        "tofa" => Some(TOFA.get_or_init(|| Quick(integrations::find("tofa").unwrap()))),
        other => integrations::find(other),
    }
}

/// A service of `kind` at `base`, fetched every second.
async fn service(app: &App, kind: &str, base: &str) -> i64 {
    let id = store::create(
        &app.magpie.core,
        &ServiceInput {
            kind: kind.into(),
            name: format!("A {kind}"),
            url: Some(base.into()),
            settings: json!({}),
            secret: SecretChange::Replace("down-test-key".into()),
            interval_s: 15,
            visibility: Visibility::Admin,
            users: Vec::new(),
        },
    )
    .await
    .unwrap();
    app.exec(format!(
        "UPDATE services SET interval_s = 1 WHERE id = {id}"
    ))
    .await;
    id
}

async fn start(app: &App) -> PollerHandle {
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

/// The snapshots the poller publishes for `id`, each once, until `done` says
/// it has seen enough or `limit` is up.
async fn watch(
    poller: &PollerHandle,
    id: i64,
    limit: Duration,
    mut done: impl FnMut(&[Arc<Snapshot>]) -> bool,
) -> Vec<Arc<Snapshot>> {
    let started = Instant::now();
    let mut seen: Vec<Arc<Snapshot>> = Vec::new();
    while started.elapsed() < limit {
        if let Some(s) = poller.snapshot(id)
            && seen
                .last()
                .is_none_or(|last| last.fetched_at != s.fetched_at)
        {
            seen.push(s);
            if done(&seen) {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    seen
}

fn keys(s: &Snapshot) -> Vec<&str> {
    s.needs.iter().map(|n| n.key.as_str()).collect()
}

fn summary() -> Json {
    json!({
        "monitors": { "up": 3, "down": 0, "pending": 0, "unknown": 0, "paused": 0 },
        "uptime": { "pct": 99.9, "pct_24h": 100.0 },
        "down": [],
    })
}

const VV_SUMMARY: &str = "/api/v1/summary";

#[tokio::test]
async fn an_app_that_stops_answering_gets_the_down_need_after_two_failures() {
    let app = App::new().await;
    let (base, stub) = stub::serve().await;
    stub.ok(VV_SUMMARY, &summary());
    let id = service(&app, "vv", &base).await;
    let poller = start(&app).await;

    let seen = watch(&poller, id, Duration::from_secs(5), |s| s.len() >= 2).await;
    assert!(
        seen.last().is_some_and(|s| s.status == Status::Ok),
        "it answers at first: {seen:?}"
    );
    assert!(seen.iter().all(|s| s.needs.is_empty()));

    // It stops answering.
    stub.route(VV_SUMMARY, Answer::Status(503));
    let seen = watch(&poller, id, Duration::from_secs(12), |s| {
        s.last().is_some_and(|s| !s.needs.is_empty())
    })
    .await;
    let failed: Vec<_> = seen.iter().filter(|s| s.status == Status::Bad).collect();
    assert!(failed.len() >= 2, "{seen:?}");
    // The first failure is not yet a need; the second is.
    assert!(failed[0].needs.is_empty(), "one failure is not a need");
    let need = &failed[1].needs;
    assert_eq!(need.len(), 1, "{need:?}");
    assert_eq!(need[0].key, DOWN_KEY);
    assert_eq!(need[0].label_key, "need.service_down");
    assert_eq!(need[0].params.len(), 0);

    // It comes back: the need goes.
    stub.ok(VV_SUMMARY, &summary());
    let seen = watch(&poller, id, Duration::from_secs(6), |s| {
        s.last().is_some_and(|s| s.status == Status::Ok)
    })
    .await;
    assert!(seen.last().unwrap().needs.is_empty());
}

#[tokio::test]
async fn an_app_that_cannot_be_reached_gets_it_too() {
    let app = App::new().await;
    // Nothing listens on this port.
    let free = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let id = service(&app, "vv", &format!("http://{free}")).await;
    let poller = start(&app).await;
    let seen = watch(&poller, id, Duration::from_secs(12), |s| {
        s.last().is_some_and(|s| !s.needs.is_empty())
    })
    .await;
    let last = seen.last().unwrap();
    assert_eq!(last.error.as_deref(), Some("connection failed"));
    assert_eq!(keys(last), [DOWN_KEY]);
    assert_eq!(last.needs[0].label_key, "need.service_down");
}

#[tokio::test]
async fn a_refused_key_is_a_config_need_and_never_the_down_need() {
    let app = App::new().await;
    let (base, stub) = stub::serve().await;
    stub.route(VV_SUMMARY, Answer::Status(401));
    let id = service(&app, "vv", &base).await;
    let poller = start(&app).await;
    let seen = watch(&poller, id, Duration::from_secs(12), |s| s.len() >= 4).await;
    assert!(seen.len() >= 3, "{seen:?}");
    for s in &seen {
        assert_eq!(s.error.as_deref(), Some("key refused"));
        assert!(!keys(s).contains(&DOWN_KEY), "{:?}", s.needs);
    }
    let last = seen.last().unwrap();
    assert_eq!(keys(last), [CONFIG_KEY]);
    assert_eq!(last.needs[0].label_key, "need.service_config");
}

#[tokio::test]
async fn tofa_with_a_health_call_that_fails_once_raises_nothing() {
    let app = App::new().await;
    let (base, stub) = stub::serve().await;
    stub.ok("/api/v1/system/playback/sessions", &json!([]));
    stub.ok("/api/v1/libraries/health", &json!([]));
    stub.ok("/api/v1/system/tasks", &json!([]));
    // The second health call fails, every other one answers.
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    stub.route(
        "/api/v1/health",
        Answer::With(Arc::new(move |_| {
            if counter.fetch_add(1, Ordering::SeqCst) == 1 {
                Answer::Status(503)
            } else {
                Answer::Json(json!({ "status": "ok", "version": "0.13.1" }))
            }
        })),
    );
    let id = service(&app, "tofa", &base).await;
    let poller = start(&app).await;
    let seen = watch(&poller, id, Duration::from_secs(10), |s| {
        s.len() >= 4 && calls.load(Ordering::SeqCst) >= 4
    })
    .await;
    assert!(
        seen.iter().any(|s| s.status == Status::Bad),
        "the failed call was seen: {seen:?}"
    );
    for s in &seen {
        assert!(s.needs.is_empty(), "{:?}", s.needs);
    }
    assert_eq!(seen.last().unwrap().status, Status::Ok);
}
