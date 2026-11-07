//! «Trenger deg» and acknowledgements: which needs a user
//! gets (`needs` in the values document), what an acknowledgement hides and
//! when it stops hiding, and the routes that make and take back an
//! acknowledgement.
//!
//! Snapshots are put in through the idle poller's `set_snapshot_for_tests`
//! (the page reads the same handle); the test of a need that goes and comes
//! back runs a real poller on a paused clock with a stub kind.

mod common;

use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderValue, Request, StatusCode, header};
use common::{App, HOST, Res, Web, bearer_get, make_token, user_id};
use futures_util::{Stream, StreamExt as _};
use jiff::Timestamp;
use mindful_magpie::integrations::poller::{Options, Poller, PollerHandle};
use mindful_magpie::integrations::{
    self, BoxFuture, FetchCtx, FetchError, Field, Integration, Level, MetricDef, Need, Snapshot,
    Status, Value as Reading,
};
use mindful_magpie::services::{SecretChange, ServiceEvent, ServiceInput, Visibility, store};
use serde_json::{Value, json};
use tokio::time::Instant;
use tower::ServiceExt as _;
use vole_kit::auth::TokenScope;

const SECRET: &str = "s3cr3t-Tok3n-needs-4k5l6m";

// ---------------------------------------------------------------------------
// Helpers

fn input(name: &str, visibility: Visibility, users: &[i64]) -> ServiceInput {
    ServiceInput {
        kind: "http".into(),
        name: name.into(),
        url: Some("http://10.55.66.77:8123/needs-check".into()),
        settings: json!({ "expected_status": "200-299" }),
        secret: SecretChange::Replace(SECRET.into()),
        interval_s: 60,
        visibility,
        users: users.to_vec(),
    }
}

async fn service(app: &App, name: &str, visibility: Visibility, users: &[i64]) -> i64 {
    store::create(&app.magpie.core, &input(name, visibility, users))
        .await
        .unwrap()
}

fn card(id: &str, service: Option<Value>) -> Value {
    let mut card = json!({
        "id": id,
        "title": "A card",
        "description": "",
        "url": "https://blog.example.com/",
        "icon": { "emoji": null, "colour": "teal" }
    });
    if let Some(service) = service {
        card["service"] = service;
    }
    card
}

fn doc(version: u64, cards: Vec<Value>) -> Value {
    json!({
        "version": version,
        "background": { "kind": "preset", "name": "hav" },
        "groups": [{ "id": "g1", "title": "Services", "cards": cards }],
        "bookmarks": []
    })
}

fn bind(id: i64) -> Value {
    json!({ "id": id, "metrics": ["status"] })
}

async fn save(web: &mut Web<'_>, layout: &Value) {
    let req = Request::post("/start/layout")
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header("hx-request", "true")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(layout).unwrap()))
        .unwrap();
    let res = web.send(req).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

/// The JSON inside `<script type="application/json" id="{id}">`.
fn block(page: &str, id: &str) -> Value {
    let open = format!(r#"<script type="application/json" id="{id}">"#);
    let start = page
        .find(&open)
        .unwrap_or_else(|| panic!("no {id} block in {page}"))
        + open.len();
    let end = page[start..]
        .to_ascii_lowercase()
        .find("</script")
        .expect("block never closes")
        + start;
    serde_json::from_str(&page[start..end]).unwrap_or_else(|e| panic!("{e}: {}", &page[start..end]))
}

async fn embed_in(web: &mut Web<'_>, lang: &str) -> Value {
    let req = Request::get("/")
        .header("accept-language", lang)
        .body(Body::empty())
        .unwrap();
    let res = web.send(req).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    block(&res.body, "mm-values")
}

async fn embed(web: &mut Web<'_>) -> Value {
    embed_in(web, "en").await
}

/// The `needs` list of the page's values block.
async fn needs_of(web: &mut Web<'_>) -> Vec<Value> {
    embed(web).await["needs"]
        .as_array()
        .unwrap_or_else(|| panic!("the values block has no needs list"))
        .clone()
}

fn need(key: &str, level: Level, score: f64) -> Need {
    Need {
        key: key.into(),
        level,
        label_key: if key == "config" {
            "need.service_config".into()
        } else {
            "need.http_down".into()
        },
        // The page shows the service's name beside the text.
        params: Default::default(),
        score,
        since: None,
    }
}

fn snap(needs: Vec<Need>) -> Snapshot {
    let mut s = Snapshot::new(Status::Bad);
    s.fetched_at = jiff::Timestamp::from_second(1_760_000_000).unwrap();
    s.values.insert("status".into(), Reading::num(500.0));
    s.needs = needs;
    s
}

/// A post from the page's own script: same-origin proof and a JSON body.
fn ack_request(path: &str, body: &Value) -> Request<Body> {
    Request::post(path)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header("hx-request", "true")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn ack(web: &mut Web<'_>, service_id: i64, key: &str) -> Res {
    web.send(ack_request(
        "/start/needs/ack",
        &json!({ "service_id": service_id, "key": key }),
    ))
    .await
}

async fn unack(web: &mut Web<'_>, service_id: i64, key: &str) -> Res {
    web.send(ack_request(
        "/start/needs/unack",
        &json!({ "service_id": service_id, "key": key }),
    ))
    .await
}

/// An admin, a viewer, and one service for everybody that both have a card for.
struct World {
    app: App,
    blog: i64,
}

async fn world() -> World {
    let app = App::new().await;
    let blog = service(&app, "Blog", Visibility::All, &[]).await;
    World { app, blog }
}

impl World {
    async fn viewer(&self, name: &'static str) -> Web<'_> {
        let mut web = Web::new(&self.app, name, "viewer").await;
        save(&mut web, &doc(0, vec![card("c1", Some(bind(self.blog)))])).await;
        web
    }

    async fn admin(&self) -> Web<'_> {
        let mut web = Web::new(&self.app, "ada", "admin").await;
        save(&mut web, &doc(0, vec![card("c1", Some(bind(self.blog)))])).await;
        web
    }

    fn set(&self, id: i64, needs: Vec<Need>) {
        self.app
            .magpie
            .poller
            .set_snapshot_for_tests(id, snap(needs));
    }

    async fn acks(&self) -> i64 {
        self.app.count("SELECT COUNT(*) FROM acks").await
    }
}

// --- the event stream ------------------------------------------------------

type Body_ = Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>;

struct Live {
    body: Body_,
    buf: String,
}

#[derive(Debug)]
enum Event {
    Values(Value),
    Ended,
    Quiet,
}

async fn request_live(web: &Web<'_>) -> axum::response::Response {
    let mut req = Request::get("/start/live")
        .header(header::HOST, HOST)
        .body(Body::empty())
        .unwrap();
    req.headers_mut().insert(
        header::COOKIE,
        HeaderValue::from_str(&web.cookie_header()).unwrap(),
    );
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:40003".parse::<std::net::SocketAddr>().unwrap(),
    ));
    web.app.router.clone().oneshot(req).await.unwrap()
}

async fn open_live(web: &Web<'_>) -> Live {
    let res = request_live(web).await;
    assert_eq!(res.status(), StatusCode::OK);
    Live {
        body: Box::pin(res.into_body().into_data_stream()),
        buf: String::new(),
    }
}

impl Live {
    async fn next(&mut self, wait: Duration) -> Event {
        let deadline = Instant::now() + wait;
        loop {
            while let Some(end) = self.buf.find("\n\n") {
                let frame: String = self.buf.drain(..end + 2).collect();
                let mut name = None;
                let mut data = String::new();
                for line in frame.lines() {
                    if let Some(rest) = line.strip_prefix("event:") {
                        name = Some(rest.trim().to_string());
                    } else if let Some(rest) = line.strip_prefix("data:") {
                        data.push_str(rest.trim_start());
                    }
                }
                if name.as_deref() == Some("values") {
                    return Event::Values(
                        serde_json::from_str(&data).unwrap_or_else(|e| panic!("{e}: {data}")),
                    );
                }
            }
            match tokio::time::timeout_at(deadline, self.body.next()).await {
                Err(_) => return Event::Quiet,
                Ok(None) => return Event::Ended,
                Ok(Some(chunk)) => self.buf.push_str(&String::from_utf8_lossy(&chunk.unwrap())),
            }
        }
    }

    async fn event(&mut self) -> Value {
        match self.next(Duration::from_secs(10)).await {
            Event::Values(v) => v,
            other => panic!("wanted an event, got {other:?}"),
        }
    }

    async fn nothing(&mut self) {
        match self.next(Duration::from_secs(5)).await {
            Event::Quiet => {}
            other => panic!("wanted silence, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// What the values document says of needs

#[tokio::test]
async fn needs_are_listed_with_a_translated_label_and_their_age() {
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    assert!(needs_of(&mut bob).await.is_empty(), "no snapshot, no need");

    w.set(
        w.blog,
        vec![
            need("disk", Level::Warn, 88.0),
            need("down", Level::Bad, 1.0),
        ],
    );
    let list = needs_of(&mut bob).await;
    assert_eq!(list.len(), 2);
    // Red before yellow.
    assert_eq!(list[0]["key"], "down");
    assert_eq!(list[0]["level"], "bad");
    assert_eq!(list[0]["service_id"], w.blog);
    assert_eq!(list[0]["service_name"], "Blog");
    assert_eq!(list[0]["label"], "down");
    assert_eq!(list[0]["acked"], false);
    // The need began with the reading that first had it.
    assert_eq!(list[0]["since"], 1_760_000_000);
    assert_eq!(list[1]["key"], "disk");
    assert_eq!(list[1]["level"], "warn");

    // In Norwegian, from the same data.
    let nb = embed_in(&mut bob, "nb").await;
    assert_eq!(nb["needs"][0]["label"], "nede");
}

#[tokio::test]
async fn a_need_carries_no_text_but_the_services_name() {
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    let mut hostile = need("down", Level::Bad, 1.0);
    // Anything else a snapshot holds is not shown: only the translated label.
    hostile
        .params
        .insert("extra".into(), "<img src=x onerror=1>".into());
    hostile.label_key = "need.no_such_text".into();
    w.set(w.blog, vec![hostile]);
    let list = needs_of(&mut bob).await;
    assert_eq!(list.len(), 1);
    let text = list[0].to_string();
    assert!(!text.contains("onerror"), "{text}");
    assert!(!text.contains("need.no_such_text"), "{text}");
    assert_eq!(list[0]["label"], "needs attention");
}

#[tokio::test]
async fn needs_come_only_from_services_that_are_bound_and_visible() {
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    let bob_id = user_id(&w.app, "bob").await;
    let unbound = service(&w.app, "Wiki", Visibility::All, &[]).await;
    let hidden = service(&w.app, "Router", Visibility::Admin, &[]).await;
    let private = service(&w.app, "Vault", Visibility::Users, &[bob_id]).await;
    let mut ada = Web::new(&w.app, "ada", "admin").await;
    save(
        &mut ada,
        &doc(
            0,
            vec![
                card("c1", Some(bind(hidden))),
                card("c2", Some(bind(private))),
            ],
        ),
    )
    .await;
    for id in [w.blog, unbound, hidden, private] {
        w.set(id, vec![need("down", Level::Bad, 1.0)]);
    }
    // Bob's card is bound to Blog only.
    let list = needs_of(&mut bob).await;
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["service_id"], w.blog);
    // Ada's cards are bound to Router and Vault, both visible to an admin.
    let mut ids: Vec<i64> = needs_of(&mut ada)
        .await
        .iter()
        .map(|n| n["service_id"].as_i64().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![hidden.min(private), hidden.max(private)]);
    // A binding that became hidden later: nothing of it, not even its name.
    let with_vault = doc(
        1,
        vec![
            card("c1", Some(bind(w.blog))),
            card("c2", Some(bind(private))),
        ],
    );
    save(&mut bob, &with_vault).await;
    assert_eq!(needs_of(&mut bob).await.len(), 2);
    store::update(
        &w.app.magpie.core,
        private,
        &input("Vault", Visibility::Admin, &[]),
    )
    .await
    .unwrap();
    let after = needs_of(&mut bob).await;
    assert_eq!(after.len(), 1);
    assert!(!after[0].to_string().contains("Vault"));
}

// ---------------------------------------------------------------------------
// The rule

#[tokio::test]
async fn an_ack_hides_the_need_for_that_user_only() {
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    let mut cy = w.viewer("cy").await;
    let mut ada = w.admin().await;
    w.set(w.blog, vec![need("down", Level::Bad, 1.0)]);

    let res = ack(&mut bob, w.blog, "down").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    // The answer is the user's new list.
    assert_eq!(res.json()["needs"][0]["acked"], true);
    assert_eq!(res.json()["needs"].as_array().unwrap().len(), 1);

    assert_eq!(needs_of(&mut bob).await[0]["acked"], true);
    // Nobody else's list changed.
    assert_eq!(needs_of(&mut cy).await[0]["acked"], false);
    assert_eq!(needs_of(&mut ada).await[0]["acked"], false);
    assert_eq!(w.acks().await, 1);
}

#[tokio::test]
async fn two_users_acknowledge_independently() {
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    let mut cy = w.viewer("cy").await;
    w.set(w.blog, vec![need("down", Level::Bad, 1.0)]);

    assert_eq!(ack(&mut bob, w.blog, "down").await.status, StatusCode::OK);
    assert_eq!(ack(&mut cy, w.blog, "down").await.status, StatusCode::OK);
    assert_eq!(w.acks().await, 2);
    // Bob takes his back; Cy's stays.
    assert_eq!(unack(&mut bob, w.blog, "down").await.status, StatusCode::OK);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], false);
    assert_eq!(needs_of(&mut cy).await[0]["acked"], true);
    assert_eq!(w.acks().await, 1);
    // Taking back what is not acknowledged is no error.
    assert_eq!(unack(&mut bob, w.blog, "down").await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_need_that_gets_worse_shows_again() {
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    w.set(w.blog, vec![need("disk", Level::Warn, 88.0)]);
    assert_eq!(ack(&mut bob, w.blog, "disk").await.status, StatusCode::OK);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], true);

    // The same level and a lower or equal score: still hidden.
    w.set(w.blog, vec![need("disk", Level::Warn, 88.0)]);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], true);
    w.set(w.blog, vec![need("disk", Level::Warn, 80.0)]);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], true);

    // The score rises (88 to 92): shown again.
    w.set(w.blog, vec![need("disk", Level::Warn, 92.0)]);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], false);

    // Acknowledged again at 92, then warn to bad at the same score.
    assert_eq!(ack(&mut bob, w.blog, "disk").await.status, StatusCode::OK);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], true);
    w.set(w.blog, vec![need("disk", Level::Bad, 92.0)]);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], false);

    // An ack at bad stays hidden when the need falls back to warn.
    assert_eq!(ack(&mut bob, w.blog, "disk").await.status, StatusCode::OK);
    w.set(w.blog, vec![need("disk", Level::Warn, 92.0)]);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], true);
}

#[tokio::test]
async fn an_ack_takes_level_and_score_from_the_server_not_the_client() {
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    w.set(w.blog, vec![need("disk", Level::Warn, 88.0)]);
    let body = json!({
        "service_id": w.blog, "key": "disk",
        "level": "bad", "score": 1_000_000.0, "acked_at": "2999-01-01T00:00:00Z"
    });
    let res = bob.send(ack_request("/start/needs/ack", &body)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let (level, score, at): (String, f64, String) = w
        .app
        .magpie
        .core
        .with_conn(|c| {
            c.query_row("SELECT level, score, acked_at FROM acks", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
        })
        .await
        .unwrap();
    assert_eq!((level.as_str(), score), ("warn", 88.0));
    assert!(at.starts_with("20") && !at.starts_with("2999"), "{at}");
    // A rise past what the server stored shows it again.
    w.set(w.blog, vec![need("disk", Level::Warn, 89.0)]);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], false);
}

#[tokio::test]
async fn a_need_that_began_after_the_ack_shows_again() {
    // The belt to the poller's braces: an ack that outlived its need (a
    // reading and an ack crossing) does not hide the need that came back.
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    w.set(w.blog, vec![need("down", Level::Bad, 1.0)]);
    assert_eq!(ack(&mut bob, w.blog, "down").await.status, StatusCode::OK);
    let mut back = need("down", Level::Bad, 1.0);
    back.since = Some(Timestamp::now() + jiff::SignedDuration::from_secs(60));
    w.set(w.blog, vec![back]);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], false);
}

// ---------------------------------------------------------------------------
// A need that goes and comes back (the poller clears acks)

static MODE: AtomicU8 = AtomicU8::new(0);
/// The tests that run a poller with `Flaky` share `MODE`: one at a time.
static MODE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Flaky;
static NO_FIELDS: [Field; 0] = [];
static NO_METRICS: [MetricDef; 0] = [];

impl Integration for Flaky {
    fn kind(&self) -> &'static str {
        "http"
    }
    fn label_key(&self) -> &'static str {
        "kind.http"
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
    fn fetch<'a>(&'a self, _ctx: &'a FetchCtx) -> BoxFuture<'a, Result<Snapshot, FetchError>> {
        Box::pin(async move {
            let mut s = Snapshot::new(Status::Ok);
            s.values.insert("status".into(), Reading::num(200.0));
            match MODE.load(Ordering::SeqCst) {
                1 => s.needs.push(need("disk", Level::Warn, 88.0)),
                2 => s.needs.push(need("other", Level::Warn, 10.0)),
                3 => s.needs.push(need("disk", Level::Warn, 92.0)),
                4 => s.needs.push(need("disk", Level::Bad, 88.0)),
                // A kind's date in the future, and a score that is no number.
                5 => {
                    let mut n = need("late", Level::Warn, f64::NAN);
                    n.since = Some(Timestamp::now() + jiff::SignedDuration::from_secs(86_400));
                    s.needs.push(n);
                }
                // A kind's date in the past: kept.
                6 => {
                    let mut n = need("early", Level::Warn, 5.0);
                    n.since = Some(Timestamp::now() - jiff::SignedDuration::from_secs(3_600));
                    s.needs.push(n);
                }
                _ => {}
            }
            Ok(s)
        })
    }
}

fn flaky_kinds(kind: &str) -> Option<&'static dyn Integration> {
    match kind {
        "http" => Some(&Flaky),
        other => integrations::find(other),
    }
}

async fn wait_poller(
    poller: &PollerHandle,
    id: i64,
    after: Option<Timestamp>,
    ok: impl Fn(&Snapshot) -> bool,
) -> std::sync::Arc<Snapshot> {
    let mut rx = poller.subscribe();
    loop {
        if let Some(s) = poller.snapshot(id)
            && after.is_none_or(|t| s.fetched_at > t)
            && ok(&s)
        {
            return s;
        }
        match tokio::time::timeout(Duration::from_secs(100_000), rx.recv()).await {
            Ok(_) => {}
            Err(_) => panic!("no matching snapshot of service {id}"),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_need_that_goes_and_comes_back_shows_again() {
    let _mode = MODE_LOCK.lock().await;
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    let mut cy = w.viewer("cy").await;
    tokio::time::pause();
    MODE.store(1, Ordering::SeqCst);
    let poller = Poller::start_with(
        w.app.magpie.core.clone(),
        integrations::client().clone(),
        &w.app.magpie.service_events,
        Options {
            kinds: flaky_kinds,
            spread: Duration::ZERO,
        },
    )
    .await;
    let first = wait_poller(&poller, w.blog, None, |s| !s.needs.is_empty()).await;
    // The poller dates a need that has no date, and keeps the date while the
    // need stays.
    let since = first.needs[0].since.expect("the poller dates the need");
    assert_eq!(since, first.fetched_at);
    let second = wait_poller(&poller, w.blog, Some(first.fetched_at), |s| {
        !s.needs.is_empty()
    })
    .await;
    assert_eq!(second.needs[0].since, Some(since), "the date is kept");

    // The page reads another handle: give it what the poller has.
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.blog, (*second).clone());
    assert_eq!(ack(&mut bob, w.blog, "disk").await.status, StatusCode::OK);
    assert_eq!(ack(&mut cy, w.blog, "disk").await.status, StatusCode::OK);
    assert_eq!(w.acks().await, 2);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], true);

    // Another need appears: the ack of "disk" stays.
    MODE.store(2, Ordering::SeqCst);
    let other = wait_poller(&poller, w.blog, Some(second.fetched_at), |s| {
        s.needs.iter().any(|n| n.key == "other")
    })
    .await;
    // "disk" is gone from the snapshot: both acks go, in the database.
    assert!(other.needs.iter().all(|n| n.key != "disk"));
    for _ in 0..200 {
        if w.acks().await == 0 {
            break;
        }
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        w.acks().await,
        0,
        "the acks of a need that went are deleted"
    );

    // It comes back: shown to both, with a new date.
    MODE.store(1, Ordering::SeqCst);
    let back = wait_poller(&poller, w.blog, Some(other.fetched_at), |s| {
        s.needs.iter().any(|n| n.key == "disk")
    })
    .await;
    assert!(back.needs[0].since > Some(since), "it began again");
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.blog, (*back).clone());
    assert_eq!(needs_of(&mut bob).await[0]["acked"], false);
    assert_eq!(needs_of(&mut cy).await[0]["acked"], false);
}

// ---------------------------------------------------------------------------
// The routes

#[tokio::test]
async fn an_ack_on_a_service_the_user_cannot_use_is_a_plain_404() {
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    let hidden = service(&w.app, "Router", Visibility::Admin, &[]).await;
    let unbound = service(&w.app, "Wiki", Visibility::All, &[]).await;
    for id in [w.blog, hidden, unbound] {
        w.set(id, vec![need("down", Level::Bad, 1.0)]);
    }
    let missing = ack(&mut bob, 987_654, "down").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    for (what, id, key) in [
        ("hidden", hidden, "down"),
        ("unbound", unbound, "down"),
        ("a need that is not there", w.blog, "nothing"),
    ] {
        let res = ack(&mut bob, id, key).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{what}: {}", res.body);
        // Not told apart from a service that does not exist.
        assert_eq!(res.body, missing.body, "{what}");
        assert_eq!(res.header("content-type"), missing.header("content-type"));
        let res = unack(&mut bob, id, key).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{what} (unack)");
        assert_eq!(res.body, missing.body, "{what} (unack)");
    }
    assert!(!missing.body.contains("Router") && !missing.body.contains("Wiki"));
    assert_eq!(w.acks().await, 0);
    // A body that is not an acknowledgement is a bad request.
    let res = bob
        .send(ack_request("/start/needs/ack", &json!({ "key": 3 })))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_ack_routes_need_the_same_origin_proof_and_a_session() {
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    w.set(w.blog, vec![need("down", Level::Bad, 1.0)]);
    let body = json!({ "service_id": w.blog, "key": "down" }).to_string();
    for path in ["/start/needs/ack", "/start/needs/unack"] {
        // Signed in, but nothing shows the request comes from our own page.
        let req = Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.clone()))
            .unwrap();
        assert_eq!(bob.send(req).await.status, StatusCode::FORBIDDEN, "{path}");
        // Another site's Origin is no proof either.
        let req = Request::post(path)
            .header(header::ORIGIN, "https://evil.example")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.clone()))
            .unwrap();
        assert_eq!(bob.send(req).await.status, StatusCode::FORBIDDEN, "{path}");
        // Not signed in: no acknowledgement either.
        let mut nobody = Web::anonymous(&w.app);
        let res = nobody
            .send(ack_request(path, &serde_json::from_str(&body).unwrap()))
            .await;
        assert!(
            res.status == StatusCode::UNAUTHORIZED
                || res.status.is_redirection()
                || res.status == StatusCode::FORBIDDEN,
            "{path}: {}",
            res.status
        );
    }
    assert_eq!(w.acks().await, 0);
    // A token is no session: these routes are the page's.
    let token = make_token(&w.app, user_id(&w.app, "bob").await, TokenScope::Write, "t").await;
    let res = w
        .app
        .send(
            Request::post("/start/needs/ack")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::ORIGIN, format!("http://{HOST}"))
                .header("hx-request", "true")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await;
    assert_ne!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(w.acks().await, 0);
    // With the page's own proof it works.
    assert_eq!(ack(&mut bob, w.blog, "down").await.status, StatusCode::OK);
    assert_eq!(w.acks().await, 1);
}

#[tokio::test]
async fn deleting_a_service_or_a_user_deletes_their_acks() {
    let w = world().await;
    let wiki = service(&w.app, "Wiki", Visibility::All, &[]).await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(
            0,
            vec![card("c1", Some(bind(w.blog))), card("c2", Some(bind(wiki)))],
        ),
    )
    .await;
    let mut cy = w.viewer("cy").await;
    for id in [w.blog, wiki] {
        w.set(id, vec![need("down", Level::Bad, 1.0)]);
    }
    for id in [w.blog, wiki] {
        assert_eq!(ack(&mut bob, id, "down").await.status, StatusCode::OK);
    }
    assert_eq!(ack(&mut cy, w.blog, "down").await.status, StatusCode::OK);
    assert_eq!(w.acks().await, 3);

    store::delete(&w.app.magpie.core, wiki).await.unwrap();
    assert_eq!(w.acks().await, 2, "the service's acks go with it");

    let cy_id = user_id(&w.app, "cy").await;
    w.app
        .exec(format!("DELETE FROM users WHERE id = {cy_id}"))
        .await;
    assert_eq!(w.acks().await, 1, "the user's acks go with them");
}

// ---------------------------------------------------------------------------
// «Config» is for admins

fn config_need() -> Need {
    need("config", Level::Bad, 1.0)
}

#[tokio::test]
async fn a_config_need_is_never_shown_to_a_non_admin() {
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    let mut ada = w.admin().await;
    w.set(w.blog, vec![config_need(), need("disk", Level::Warn, 90.0)]);

    // The page.
    let page = embed(&mut bob).await;
    let keys: Vec<&str> = page["needs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["disk"]);
    assert!(!page.to_string().contains("set up again"), "{page}");
    // The admin gets it.
    let admin_keys: Vec<String> = needs_of(&mut ada)
        .await
        .iter()
        .map(|n| n["key"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(admin_keys, vec!["config", "disk"]);

    // The API (a plain read token is a viewer's, even an admin's).
    let bob_token = make_token(&w.app, user_id(&w.app, "bob").await, TokenScope::Read, "b").await;
    let res = bearer_get(&w.app, "/api/v1/me/values", &bob_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let api = res.json();
    assert_eq!(api["needs"].as_array().unwrap().len(), 1);
    assert!(
        !res.body.contains("config") && !res.body.contains("set up again"),
        "{}",
        res.body
    );
    let ada_token = make_token(&w.app, user_id(&w.app, "ada").await, TokenScope::Read, "a").await;
    let res = bearer_get(&w.app, "/api/v1/me/values", &ada_token).await;
    assert_eq!(res.json()["needs"].as_array().unwrap().len(), 1);

    // The stream, first event and later ones.
    let mut live = open_live(&bob).await;
    let first = live.event().await;
    assert_eq!(first["needs"].as_array().unwrap().len(), 1, "{first}");
    assert!(!first.to_string().contains("set up again"), "{first}");
    tokio::time::pause();
    w.set(w.blog, vec![config_need()]);
    // Only the config need changed for Bob: nothing worth sending... except
    // that his list is now empty.
    let update = live.event().await;
    assert_eq!(update["needs"], json!([]), "{update}");
    assert!(!update.to_string().contains("set up again"), "{update}");
    live.nothing().await;
    // An admin's stream says it.
    let mut admin_live = open_live(&ada).await;
    let first = admin_live.event().await;
    assert_eq!(first["needs"][0]["key"], "config", "{first}");

    // No acknowledgement of it by a non-admin, not told apart from a missing need.
    let denied = ack(&mut bob, w.blog, "config").await;
    let missing = ack(&mut bob, w.blog, "nothing").await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND);
    assert_eq!(denied.body, missing.body);
    assert_eq!(w.acks().await, 0);
    assert_eq!(ack(&mut ada, w.blog, "config").await.status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Live

#[tokio::test]
async fn an_ack_reaches_the_open_streams_of_that_user_only() {
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    let cy = w.viewer("cy").await;
    w.set(w.blog, vec![need("down", Level::Bad, 1.0)]);
    let mut tab1 = open_live(&bob).await;
    let mut tab2 = open_live(&bob).await;
    let mut other = open_live(&cy).await;
    for live in [&mut tab1, &mut tab2, &mut other] {
        let first = live.event().await;
        assert_eq!(first["full"], true);
        assert_eq!(first["needs"][0]["acked"], false);
    }
    tokio::time::pause();

    let res = ack(&mut bob, w.blog, "down").await;
    assert_eq!(res.status, StatusCode::OK);
    for live in [&mut tab1, &mut tab2] {
        let update = live.event().await;
        assert_eq!(update["full"], false);
        assert_eq!(update["needs"][0]["acked"], true, "{update}");
        assert_eq!(update["services"], json!({}), "only the needs changed");
        live.nothing().await;
    }
    other.nothing().await;

    // Taking it back is live too.
    unack(&mut bob, w.blog, "down").await;
    let update = tab1.event().await;
    assert_eq!(update["needs"][0]["acked"], false);
}

#[tokio::test]
async fn a_need_that_appears_or_goes_is_an_event() {
    let w = world().await;
    let bob = w.viewer("bob").await;
    w.set(w.blog, vec![]);
    let mut live = open_live(&bob).await;
    assert_eq!(live.event().await["needs"], json!([]));
    tokio::time::pause();
    w.set(w.blog, vec![need("down", Level::Bad, 1.0)]);
    let update = live.event().await;
    assert_eq!(update["needs"][0]["key"], "down");
    w.set(w.blog, vec![]);
    assert_eq!(live.event().await["needs"], json!([]));
}

// ---------------------------------------------------------------------------
// The service list the page keeps for the editor's picker

fn visible_ids(event: &Value) -> Vec<i64> {
    let mut ids: Vec<i64> = event["services_visible"]
        .as_array()
        .unwrap_or_else(|| panic!("no services_visible in {event}"))
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

#[tokio::test]
async fn the_stream_names_every_visible_service_not_only_the_bound_ones() {
    let w = world().await;
    let bob = w.viewer("bob").await;
    let wiki = service(&w.app, "Wiki", Visibility::All, &[]).await;
    let hidden = service(&w.app, "Router", Visibility::Admin, &[]).await;
    let mut live = open_live(&bob).await;
    let first = live.event().await;
    let mut want = vec![w.blog, wiki];
    want.sort_unstable();
    assert_eq!(visible_ids(&first), want);
    let added: Vec<i64> = first["services_add"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_i64().unwrap())
        .collect();
    assert!(added.contains(&wiki) && !added.contains(&hidden), "{first}");
    tokio::time::pause();

    // Unbinding Blog drops its numbers, but it stays in the list.
    let mut bob = bob;
    save(&mut bob, &doc(1, vec![card("c1", None)])).await;
    // (A saved layout wakes no stream by itself: the next reading does.)
    w.set(w.blog, vec![]);
    let update = live.event().await;
    assert_eq!(update["removed"], json!([w.blog]));
    assert!(
        update.get("services_visible").is_none() || visible_ids(&update) == want,
        "{update}"
    );

    // Hiding Wiki from Bob takes it out of the list, though no card used it.
    store::update(
        &w.app.magpie.core,
        wiki,
        &input("Wiki", Visibility::Admin, &[]),
    )
    .await
    .unwrap();
    w.app
        .magpie
        .service_events
        .send(ServiceEvent::Updated(wiki))
        .ok();
    let update = live.event().await;
    assert_eq!(visible_ids(&update), vec![w.blog]);

    // And a service that becomes visible is added.
    store::update(
        &w.app.magpie.core,
        hidden,
        &input("Router", Visibility::All, &[]),
    )
    .await
    .unwrap();
    w.app
        .magpie
        .service_events
        .send(ServiceEvent::Updated(hidden))
        .ok();
    let update = live.event().await;
    let mut want = vec![w.blog, hidden];
    want.sort_unstable();
    assert_eq!(visible_ids(&update), want);
    assert_eq!(update["services_add"][0]["id"], hidden);
}

// ---------------------------------------------------------------------------
// The cap on one user's streams is per running app

#[tokio::test]
async fn the_cap_on_a_users_streams_is_per_app_so_tests_of_one_user_number_do_not_share_it() {
    // Every app numbers its users from 1: in one test process the same user id
    // is in many apps at once, and their streams must not count together.
    let a = App::new().await;
    let b = App::new().await;
    let bob_a = Web::new(&a, "bob", "viewer").await;
    let bob_b = Web::new(&b, "bob", "viewer").await;
    assert_eq!(user_id(&a, "bob").await, user_id(&b, "bob").await);
    let mut open = Vec::new();
    for _ in 0..mindful_magpie::values::MAX_STREAMS_PER_USER {
        let res = request_live(&bob_a).await;
        assert_eq!(res.status(), StatusCode::OK);
        open.push(res);
    }
    // Bob is at his own cap in the first app, and still has all his places in the second.
    assert_eq!(
        request_live(&bob_a).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let res = request_live(&bob_b).await;
    assert_eq!(res.status(), StatusCode::OK);
    open.push(res);
}

// ---------------------------------------------------------------------------
// An acknowledgement is spent once it stops hiding

async fn flaky_poller(w: &World) -> PollerHandle {
    Poller::start_with(
        w.app.magpie.core.clone(),
        integrations::client().clone(),
        &w.app.magpie.service_events,
        Options {
            kinds: flaky_kinds,
            spread: Duration::ZERO,
        },
    )
    .await
}

/// Waits for a reading after `after` that has the need `key` with `level` and `score`.
async fn wait_need(
    poller: &PollerHandle,
    w: &World,
    after: Option<Timestamp>,
    key: &str,
    level: Level,
    score: f64,
) -> std::sync::Arc<Snapshot> {
    let key = key.to_string();
    let s = wait_poller(poller, w.blog, after, move |s| {
        s.needs
            .iter()
            .any(|n| n.key == key && n.level == level && n.score == score)
    })
    .await;
    // The page reads another handle: give it what the poller has.
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.blog, (*s).clone());
    s
}

#[tokio::test(flavor = "current_thread")]
async fn a_need_that_got_worse_and_then_eased_stays_shown() {
    let _mode = MODE_LOCK.lock().await;
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    tokio::time::pause();
    MODE.store(1, Ordering::SeqCst);
    let poller = flaky_poller(&w).await;

    // Acknowledged at 88; the next reading is the same: it stays hidden.
    let s1 = wait_need(&poller, &w, None, "disk", Level::Warn, 88.0).await;
    assert_eq!(ack(&mut bob, w.blog, "disk").await.status, StatusCode::OK);
    let s2 = wait_need(&poller, &w, Some(s1.fetched_at), "disk", Level::Warn, 88.0).await;
    assert_eq!(w.acks().await, 1, "an unchanged need keeps the ack");
    assert_eq!(needs_of(&mut bob).await[0]["acked"], true);

    // The score rises to 92: the ack is spent.
    MODE.store(3, Ordering::SeqCst);
    let s3 = wait_need(&poller, &w, Some(s2.fetched_at), "disk", Level::Warn, 92.0).await;
    assert_eq!(
        w.acks().await,
        0,
        "the ack of a need that got worse is deleted"
    );
    assert_eq!(needs_of(&mut bob).await[0]["acked"], false);

    // It eases to 88 again: still shown, the ack does not come back.
    MODE.store(1, Ordering::SeqCst);
    let s4 = wait_need(&poller, &w, Some(s3.fetched_at), "disk", Level::Warn, 88.0).await;
    assert_eq!(needs_of(&mut bob).await[0]["acked"], false);

    // The same for the level: acknowledged at warn, it turns bad, then warn again.
    assert_eq!(ack(&mut bob, w.blog, "disk").await.status, StatusCode::OK);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], true);
    MODE.store(4, Ordering::SeqCst);
    let s5 = wait_need(&poller, &w, Some(s4.fetched_at), "disk", Level::Bad, 88.0).await;
    assert_eq!(w.acks().await, 0);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], false);
    MODE.store(1, Ordering::SeqCst);
    wait_need(&poller, &w, Some(s5.fetched_at), "disk", Level::Warn, 88.0).await;
    assert_eq!(
        needs_of(&mut bob).await[0]["acked"],
        false,
        "still shown after it eased"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn the_poller_cuts_a_future_date_back_and_makes_a_score_that_is_no_number_zero() {
    let _mode = MODE_LOCK.lock().await;
    let w = world().await;
    tokio::time::pause();
    MODE.store(5, Ordering::SeqCst);
    let poller = flaky_poller(&w).await;
    let s = wait_poller(&poller, w.blog, None, |s| !s.needs.is_empty()).await;
    assert_eq!(s.needs[0].key, "late");
    assert_eq!(
        s.needs[0].since,
        Some(s.fetched_at),
        "no later than the reading"
    );
    assert_eq!(s.needs[0].score, 0.0);

    MODE.store(6, Ordering::SeqCst);
    let s = wait_poller(&poller, w.blog, Some(s.fetched_at), |s| {
        s.needs.iter().any(|n| n.key == "early")
    })
    .await;
    let since = s.needs[0].since.unwrap();
    assert!(
        since < s.fetched_at,
        "a date before the reading is the kind's to give"
    );
}

#[tokio::test]
async fn a_score_that_is_no_number_is_stored_as_zero_and_still_hides() {
    let w = world().await;
    let mut bob = w.viewer("bob").await;
    w.set(w.blog, vec![need("odd", Level::Warn, f64::NAN)]);
    assert_eq!(ack(&mut bob, w.blog, "odd").await.status, StatusCode::OK);
    let stored: f64 = w
        .app
        .magpie
        .core
        .with_conn(|c| c.query_row("SELECT score FROM acks", [], |r| r.get(0)))
        .await
        .unwrap();
    assert_eq!(stored, 0.0);
    assert_eq!(needs_of(&mut bob).await[0]["acked"], true);
    // And straight through the table function.
    let bob_id = user_id(&w.app, "bob").await;
    mindful_magpie::needs::ack(
        &w.app.magpie.core,
        bob_id,
        w.blog,
        "inf",
        Level::Bad,
        f64::INFINITY,
    )
    .await
    .unwrap();
    let acks = mindful_magpie::needs::acks_of(&w.app.magpie.core, bob_id)
        .await
        .unwrap();
    assert_eq!(acks[&(w.blog, "inf".to_string())].score, 0.0);
}

// ---------------------------------------------------------------------------
// A rename reaches the picker

#[tokio::test]
async fn a_renamed_service_that_no_card_uses_is_sent_again_with_its_new_name() {
    let w = world().await;
    let bob = w.viewer("bob").await;
    let wiki = service(&w.app, "Wiki", Visibility::All, &[]).await;
    let mut live = open_live(&bob).await;
    live.event().await;
    tokio::time::pause();
    store::update(
        &w.app.magpie.core,
        wiki,
        &input("Handbook", Visibility::All, &[]),
    )
    .await
    .unwrap();
    w.app
        .magpie
        .service_events
        .send(ServiceEvent::Updated(wiki))
        .ok();
    let update = live.event().await;
    let added: Vec<(i64, String)> = update["services_add"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["id"].as_i64().unwrap(),
                v["name"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(added, vec![(wiki, "Handbook".to_string())]);
    // The set of visible services did not change.
    assert!(update.get("services_visible").is_none(), "{update}");
    live.nothing().await;
}
