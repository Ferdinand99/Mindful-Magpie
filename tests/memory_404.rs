//! The memory of an address that answered 404 on `/summary` (an app that was
//! too old): it is short, and saving or testing the service makes the kind ask
//! again at once, so an app that was upgraded right after it was added is not
//! shown as too old.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use common::{App, Web};
use mindful_magpie::integrations::poller::{Options, Poller};
use mindful_magpie::integrations::{self, FetchError, Integration, Status};
use mindful_magpie::services::{SecretChange, ServiceEvent, ServiceInput, Visibility, store};
use serde_json::json;

#[derive(Default)]
struct Upgrade {
    /// Whether the app has been upgraded: `/summary` exists.
    done: AtomicBool,
    hits: AtomicUsize,
}

async fn summary(State(upgrade): State<Arc<Upgrade>>) -> Response {
    upgrade.hits.fetch_add(1, Ordering::SeqCst);
    if upgrade.done.load(Ordering::SeqCst) {
        axum::Json(json!({
            "hours": 24,
            "totals": {
                "projects": 1, "new_issues": 0, "regressed_issues": 0, "unresolved_issues": 2,
                "crash_free_sessions": 99.5, "sessions": 10, "visitors_today": 3,
                "app_users_today": 1,
            },
            "projects": [],
        }))
        .into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// A Keen Kestrel stand-in that is too old until `done` is set.
async fn stub() -> (String, Arc<Upgrade>) {
    let upgrade = Arc::new(Upgrade::default());
    let app = Router::new().fallback(summary).with_state(upgrade.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/"), upgrade)
}

fn kk() -> &'static dyn Integration {
    integrations::find("kk").expect("the kk kind is registered")
}

fn input(url: &str) -> ServiceInput {
    ServiceInput {
        kind: "kk".into(),
        name: "Errors".into(),
        url: Some(url.into()),
        settings: json!({}),
        secret: SecretChange::Replace("kk_Tok3n-abc123".into()),
        interval_s: 60,
        visibility: Visibility::Admin,
        users: Vec::new(),
    }
}

/// What the page posts for the same service, the key left alone.
fn form(url: &str) -> Vec<(&str, &str)> {
    vec![
        ("kind", "kk"),
        ("name", "Errors"),
        ("url", url),
        ("interval", "60"),
        ("visibility", "admin"),
        ("secret", ""),
    ]
}

async fn fetch(app: &App, id: i64) -> Result<integrations::Snapshot, FetchError> {
    let service = store::get(&app.magpie.core, id).await.unwrap().unwrap();
    let ctx = store::fetch_ctx(&app.magpie.core, &service).await.unwrap();
    kk().fetch(&ctx).await
}

/// A service that has been seen too old once, and an app that has been
/// upgraded since.
async fn too_old_then_upgraded(app: &App) -> (i64, String, Arc<Upgrade>) {
    let (url, upgrade) = stub().await;
    let id = store::create(&app.magpie.core, &input(&url)).await.unwrap();
    assert_eq!(fetch(app, id).await.unwrap_err(), FetchError::KkTooOld);
    upgrade.done.store(true, Ordering::SeqCst);
    // Remembered: it is not even asked.
    assert_eq!(fetch(app, id).await.unwrap_err(), FetchError::KkTooOld);
    assert_eq!(upgrade.hits.load(Ordering::SeqCst), 1);
    (id, url, upgrade)
}

#[tokio::test]
async fn saving_the_service_makes_the_kind_ask_again() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let (id, url, upgrade) = too_old_then_upgraded(&app).await;
    let res = admin
        .post(&format!("/settings/services/{id}"), &form(&url))
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let snapshot = fetch(&app, id).await.expect("it answers now");
    assert_eq!(snapshot.status, Status::Ok);
    assert_eq!(upgrade.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_makes_the_kind_ask_again() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let (id, url, upgrade) = too_old_then_upgraded(&app).await;
    let id_text = id.to_string();
    let mut f = form(&url);
    f.push(("id", &id_text));
    let res = admin.post("/settings/services/test", &f).await;
    assert!(res.body.contains("data-test-status=\"ok\""), "{}", res.body);
    assert_eq!(upgrade.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_service_event_makes_the_poller_ask_again() {
    let app = App::new().await;
    let (url, upgrade) = stub().await;
    let id = store::create(&app.magpie.core, &input(&url)).await.unwrap();
    let poller = Poller::start_with(
        app.magpie.core.clone(),
        integrations::client().clone(),
        &app.magpie.service_events,
        Options {
            kinds: integrations::find,
            spread: Duration::ZERO,
        },
    );
    // The first reading: too old.
    wait_for(&poller, id, |s| s.status == Status::Bad).await;
    upgrade.done.store(true, Ordering::SeqCst);
    let _ = app.magpie.service_events.send(ServiceEvent::Updated(id));
    wait_for(&poller, id, |s| s.status == Status::Ok).await;
}

async fn wait_for(
    poller: &mindful_magpie::integrations::poller::PollerHandle,
    id: i64,
    ok: impl Fn(&integrations::Snapshot) -> bool,
) {
    for _ in 0..200 {
        if poller.snapshot(id).is_some_and(|s| ok(&s)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no matching reading of service {id}");
}
