//! Service values on the start page: the `#mm-values` embed,
//! the `GET /start/live` event stream and `GET /api/v1/me/values`.
//!
//! The stream tests run on a paused clock (the stream waits a quiet second
//! after each change) and put snapshots in through the idle poller's
//! `set_snapshot_for_tests`. Every test that reads the stream drops it before
//! it ends, so the process-wide cap on streams is never what a test sees.

mod common;

use std::pin::Pin;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderValue, Request, StatusCode, header};
use common::{App, HOST, Res, Web, bearer_get, make_token, user_id};
use futures_util::{Stream, StreamExt as _};
use mindful_magpie::integrations::block::ListItem;
use mindful_magpie::integrations::{Block, Snapshot, Status, Value as Reading};
use mindful_magpie::services::{SecretChange, ServiceEvent, ServiceInput, Visibility, store};
use serde_json::{Value, json};
use tokio::time::Instant;
use tower::ServiceExt as _;
use vole_kit::auth::TokenScope;

const SECRET: &str = "s3cr3t-Tok3n-values-9z8y7x";
const URL_HOST: &str = "10.55.66.77";
const URL: &str = "http://10.55.66.77:8123/values-check-path";
const SETTING: &str = "260-261";
/// Userinfo that a service's address must never carry onto a page.
const USERINFO: &str = "hunter2-userinfo";

// ---------------------------------------------------------------------------
// Helpers

fn input(name: &str, visibility: Visibility, users: &[i64]) -> ServiceInput {
    ServiceInput {
        kind: "http".into(),
        name: name.into(),
        url: Some(URL.into()),
        settings: json!({ "expected_status": SETTING }),
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

/// Changes who sees the service (and announces it, as the Settings page does).
async fn set_visibility(app: &App, id: i64, name: &str, visibility: Visibility, users: &[i64]) {
    store::update(&app.magpie.core, id, &input(name, visibility, users))
        .await
        .unwrap();
    app.magpie
        .service_events
        .send(ServiceEvent::Updated(id))
        .ok();
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

fn bind(id: i64, metrics: &[&str]) -> Value {
    json!({ "id": id, "metrics": metrics })
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

async fn page(web: &mut Web<'_>, lang: &str) -> Res {
    let req = Request::get("/")
        .header("accept-language", lang)
        .body(Body::empty())
        .unwrap();
    let res = web.send(req).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    res
}

async fn embed(web: &mut Web<'_>) -> Value {
    block(&page(web, "en").await.body, "mm-values")
}

fn snap(status: Status, latency: f64) -> Snapshot {
    let mut s = Snapshot::new(status);
    s.fetched_at = jiff::Timestamp::from_second(1_760_000_000).unwrap();
    s.values.insert("status".into(), Reading::num(200.0));
    s.values.insert("latency_ms".into(), Reading::num(latency));
    s
}

fn ids(values: &Value) -> Vec<String> {
    let mut ids: Vec<String> = values["services"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    ids.sort();
    ids
}

/// Service ids as `ids` lists them: as text, in text order.
fn sorted<const N: usize>(ids: [i64; N]) -> Vec<String> {
    let mut ids: Vec<String> = ids.iter().map(i64::to_string).collect();
    ids.sort();
    ids
}

fn assert_clean(text: &str, what: &str) {
    // The service's host and port are its card link and may be shown; its
    // path, settings, key and any user name are not.
    for needle in [
        SECRET,
        "values-check-path",
        SETTING,
        "expected_status",
        USERINFO,
        "Bearer",
    ] {
        assert!(!text.contains(needle), "{what} leaks {needle:?}: {text}");
    }
}

// --- the event stream ------------------------------------------------------

type Body_ = Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>;

/// One open `/start/live`.
struct Live {
    body: Body_,
    buf: String,
}

#[derive(Debug)]
enum Event {
    Values(Value),
    /// The server closed the stream.
    Ended,
    /// Nothing came in time.
    Quiet,
}

/// Opens the stream as `web`; the answer is checked to be a stream.
async fn open_live(web: &Web<'_>, lang: &str) -> Live {
    let res = request_live(web, lang).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "text/event-stream");
    Live {
        body: Box::pin(res.into_body().into_data_stream()),
        buf: String::new(),
    }
}

async fn request_live(web: &Web<'_>, lang: &str) -> axum::response::Response {
    let mut req = Request::get("/start/live")
        .header(header::HOST, HOST)
        .header("accept-language", lang)
        .body(Body::empty())
        .unwrap();
    if !web.cookies.is_empty() {
        req.headers_mut().insert(
            header::COOKIE,
            HeaderValue::from_str(&web.cookie_header()).unwrap(),
        );
    }
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:40001".parse::<std::net::SocketAddr>().unwrap(),
    ));
    web.app.router.clone().oneshot(req).await.unwrap()
}

impl Live {
    /// The next `values` event within `wait` (comments are skipped).
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
                Ok(Some(chunk)) => {
                    self.buf.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
                }
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

    async fn ended(&mut self) {
        match self.next(Duration::from_secs(10)).await {
            Event::Ended => {}
            other => panic!("wanted the stream to end, got {other:?}"),
        }
    }
}

/// An admin, a viewer and three services: `a` and `b` are for everybody, `c`
/// is for everybody too but no card is bound to it.
struct World {
    app: App,
    a: i64,
    b: i64,
    c: i64,
}

async fn world() -> World {
    let app = App::new().await;
    let a = service(&app, "Blog", Visibility::All, &[]).await;
    let b = service(&app, "Wiki", Visibility::All, &[]).await;
    let c = service(&app, "Printer", Visibility::All, &[]).await;
    World { app, a, b, c }
}

// ---------------------------------------------------------------------------
// The embed

#[tokio::test]
async fn the_page_embeds_only_bound_services_the_user_can_see() {
    let w = world().await;
    let mut ada = Web::new(&w.app, "ada", "admin").await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    let bob_id = user_id(&w.app, "bob").await;
    let secret_id = service(&w.app, "Vault", Visibility::Users, &[bob_id]).await;
    let admin_only = service(&w.app, "Router", Visibility::Admin, &[]).await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Ok, 12.0));
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.c, snap(Status::Ok, 99.0));
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(secret_id, snap(Status::Ok, 5.0));

    // Bob binds Blog and Vault (both visible to him), nothing else.
    save(
        &mut bob,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status", "latency_ms"]))),
                card("c2", Some(bind(secret_id, &["status"]))),
                card("c3", None),
            ],
        ),
    )
    .await;
    // Ada binds the admin-only service and the printer.
    save(
        &mut ada,
        &doc(
            0,
            vec![
                card("c1", Some(bind(admin_only, &["status"]))),
                card("c2", Some(bind(w.c, &["status"]))),
            ],
        ),
    )
    .await;

    let bobs = embed(&mut bob).await;
    assert_eq!(ids(&bobs), sorted([w.a, secret_id]));
    let adas = embed(&mut ada).await;
    assert_eq!(ids(&adas), sorted([admin_only, w.c]));

    // A service he may see but has no card for is in the list the card form
    // offers (`#mm-services`), not in the values; one he may not see is nowhere.
    let body = page(&mut bob, "en").await.body;
    let in_values = block(&body, "mm-values").to_string();
    for name in ["Wiki", "Printer", "Router"] {
        assert!(!in_values.contains(name), "{name} is in Bob's values");
    }
    assert!(
        !body.contains("Router"),
        "an admin-only name is on Bob's page"
    );
    assert_clean(&body, "bob's page");
    assert_clean(&page(&mut ada, "en").await.body, "ada's page");
}

#[tokio::test]
async fn a_binding_to_a_service_that_became_hidden_says_nothing() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.b, &["status"])))]),
    )
    .await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.b, snap(Status::Ok, 7.0));
    assert_eq!(ids(&embed(&mut bob).await), vec![w.b.to_string()]);

    // The save keeps an unchanged binding, so the layout still points at it.
    set_visibility(&w.app, w.b, "Wiki", Visibility::Admin, &[]).await;
    let res = page(&mut bob, "en").await;
    let values = block(&res.body, "mm-values");
    assert_eq!(values["services"], json!({}));
    assert!(!res.body.contains("Wiki"), "the hidden name is on the page");

    // A deleted service is the same.
    store::delete(&w.app.magpie.core, w.b).await.unwrap();
    assert_eq!(embed(&mut bob).await["services"], json!({}));
}

#[tokio::test]
async fn an_entry_carries_the_snapshot_and_a_translated_reason() {
    let w = world().await;
    let mut ada = Web::new(&w.app, "ada", "admin").await;
    save(
        &mut ada,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status", "latency_ms"]))),
                card("c2", Some(bind(w.b, &["status"]))),
                card("c3", Some(bind(w.c, &["status"]))),
            ],
        ),
    )
    .await;
    // A has a snapshot, B a failed one, C none yet.
    let mut good = snap(Status::Warn, 88.5);
    good.values.insert("note".into(), Reading::text("slow"));
    w.app.magpie.poller.set_snapshot_for_tests(w.a, good);
    let mut bad = snap(Status::Bad, 0.0);
    bad.error = Some("timeout".into());
    w.app.magpie.poller.set_snapshot_for_tests(w.b, bad);

    let values = embed(&mut ada).await;
    assert!(values["now"].as_i64().unwrap() > 1_700_000_000);
    let a = &values["services"][w.a.to_string()];
    assert_eq!(a["name"], "Blog");
    assert_eq!(a["kind"], "http");
    assert_eq!(a["interval_s"], 60);
    assert_eq!(a["status"], "warn");
    assert_eq!(a["fetched_at"], 1_760_000_000);
    assert_eq!(a["values"]["latency_ms"]["num"], 88.5);
    assert_eq!(a["values"]["note"]["text"], "slow");
    // Needs are the document's list (per user), not an entry's.
    assert!(a.get("needs").is_none(), "{a}");
    assert_eq!(values["needs"], json!([]));
    assert!(a.get("error_label").is_none(), "{a}");

    let b = &values["services"][w.b.to_string()];
    assert_eq!(b["status"], "bad");
    assert_eq!(b["error_label"], "Timeout");
    assert!(
        !serde_json::to_string(b).unwrap().contains("\"error\""),
        "the raw error never goes out: {b}"
    );

    // No snapshot yet: the entry is there, and waiting.
    let c = &values["services"][w.c.to_string()];
    assert_eq!(c["name"], "Printer");
    assert_eq!(c["fetched_at"], Value::Null);
    assert_eq!(c["status"], "unknown");
    assert_eq!(c["values"], json!({}));
    assert!(c.get("needs").is_none(), "{c}");
}

#[tokio::test]
async fn reasons_are_translated_and_nothing_else_gets_through() {
    let w = world().await;
    let mut ada = Web::new(&w.app, "ada", "admin").await;
    save(
        &mut ada,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status"]))),
                card("c2", Some(bind(w.b, &["status"]))),
                card("c3", Some(bind(w.c, &["status"]))),
            ],
        ),
    )
    .await;
    let reasons = [
        (w.a, "timeout"),
        (w.b, "HTTP 503"),
        // Not one of the fixed texts: it may hold anything, so it is not shown.
        (
            w.c,
            "error sending request for url (http://user:hunter2-userinfo@10.55.66.77/)",
        ),
    ];
    for (id, text) in reasons {
        let mut bad = snap(Status::Bad, 0.0);
        bad.error = Some(text.into());
        w.app.magpie.poller.set_snapshot_for_tests(id, bad);
    }
    let en = block(&page(&mut ada, "en").await.body, "mm-values");
    let nb_page = page(&mut ada, "nb").await.body;
    let nb = block(&nb_page, "mm-values");
    let label = |v: &Value, id: i64| v["services"][id.to_string()]["error_label"].clone();
    assert_eq!(label(&en, w.a), "Timeout");
    assert_eq!(label(&nb, w.a), "Tidsavbrudd");
    assert_eq!(label(&en, w.b), "HTTP 503");
    assert_eq!(label(&en, w.c), "The request failed");
    assert_clean(&serde_json::to_string(&en).unwrap(), "english values");
    assert_clean(&nb_page, "norwegian page");
}

// ---------------------------------------------------------------------------
// The API

#[tokio::test]
async fn the_values_api_needs_a_token_and_matches_the_embed() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    let bob_id = user_id(&w.app, "bob").await;
    let mine = service(&w.app, "Mine", Visibility::Users, &[bob_id]).await;
    let theirs = service(&w.app, "Theirs", Visibility::Users, &[]).await;
    for id in [w.a, mine, theirs] {
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(id, snap(Status::Ok, 3.0));
    }
    save(
        &mut bob,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status"]))),
                card("c2", Some(bind(mine, &["status"]))),
            ],
        ),
    )
    .await;
    let path = "/api/v1/me/values";

    // A cookie is not enough, and neither is nothing or a bad token.
    let res = bob.get(path).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
    assert_eq!(w.app.get(path).await.status, StatusCode::UNAUTHORIZED);
    let junk = format!("mm_{}", "x".repeat(43));
    assert_eq!(
        bearer_get(&w.app, path, &junk).await.status,
        StatusCode::UNAUTHORIZED
    );

    for scope in [TokenScope::Read, TokenScope::Write] {
        let token = make_token(&w.app, bob_id, scope, "t").await;
        let res = bearer_get(&w.app, path, &token).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        assert_eq!(res.header("cache-control"), "no-store");
        assert!(res.header("content-type").starts_with("application/json"));
        let mut api = res.json();
        let mut page = embed(&mut bob).await;
        // The clock is the only thing that may differ.
        assert!(api["now"].is_i64());
        api["now"] = json!(0);
        page["now"] = json!(0);
        assert_eq!(api, page);
        assert_eq!(ids(&api), sorted([w.a, mine]));
        assert_clean(&res.body, "the values API");
        assert!(!res.body.contains("Theirs"));
        w.app.exec("DELETE FROM api_tokens".into()).await;
    }
}

#[tokio::test]
async fn the_values_api_is_in_the_reference() {
    let w = world().await;
    let mut ada = Web::new(&w.app, "ada", "admin").await;
    let res = ada.get("/api-docs").await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("/api/v1/me/values"), "not in /api-docs");
}

#[tokio::test]
async fn a_service_with_userinfo_in_its_address_never_shows_it() {
    // The address is a column, not part of any value: whatever a kind puts in
    // an error or a value is the kind's own business, but the embed, the
    // stream and the API are built from the snapshot and the summary only.
    let w = world().await;
    let mut ada = Web::new(&w.app, "ada", "admin").await;
    let token = make_token(&w.app, user_id(&w.app, "ada").await, TokenScope::Read, "t").await;
    save(
        &mut ada,
        &doc(0, vec![card("c1", Some(bind(w.a, &["status"])))]),
    )
    .await;
    let mut s = snap(Status::Bad, 1.0);
    s.error = Some(format!("http://user:{USERINFO}@{URL_HOST}/ failed"));
    w.app.magpie.poller.set_snapshot_for_tests(w.a, s);
    assert_clean(&page(&mut ada, "en").await.body, "page");
    assert_clean(
        &bearer_get(&w.app, "/api/v1/me/values", &token).await.body,
        "api",
    );
}

// ---------------------------------------------------------------------------
// The stream

#[tokio::test]
async fn the_stream_is_for_a_signed_in_session_only() {
    let w = world().await;
    let anonymous = Web::anonymous(&w.app);
    let res = request_live(&anonymous, "en").await;
    assert_ne!(res.status(), StatusCode::OK);
    assert!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .is_none_or(|t| t != "text/event-stream")
    );

    // A token does not open it: the guard looks at cookies outside /api/.
    let ada = Web::new(&w.app, "ada", "admin").await;
    let token = make_token(&w.app, user_id(&w.app, "ada").await, TokenScope::Write, "t").await;
    let res = w
        .app
        .send(
            Request::get("/start/live")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_ne!(res.status, StatusCode::OK, "{}", res.body);
    drop(ada);
}

#[tokio::test(start_paused = false)]
async fn a_stream_starts_with_the_whole_document_of_what_is_bound() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["status"])))]),
    )
    .await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Ok, 10.0));
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.c, snap(Status::Ok, 10.0));

    let mut live = open_live(&bob, "en").await;
    let first = live.event().await;
    assert_eq!(first["full"], true);
    assert_eq!(first["removed"], json!([]));
    assert_eq!(ids(&first), vec![w.a.to_string()]);
    assert_eq!(first["services"][w.a.to_string()]["name"], "Blog");
    assert!(first["now"].is_i64());
    assert_clean(&first.to_string(), "first event");
}

#[tokio::test]
async fn a_change_to_a_bound_service_is_one_small_event() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status"]))),
                card("c2", Some(bind(w.b, &["status"]))),
            ],
        ),
    )
    .await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Ok, 10.0));
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.b, snap(Status::Ok, 20.0));
    let mut live = open_live(&bob, "en").await;
    assert_eq!(ids(&live.event().await).len(), 2);
    tokio::time::pause();

    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Warn, 450.0));
    let update = live.event().await;
    assert_eq!(update["full"], false);
    assert_eq!(update["removed"], json!([]));
    assert_eq!(ids(&update), vec![w.a.to_string()]);
    let a = &update["services"][w.a.to_string()];
    assert_eq!(a["status"], "warn");
    assert_eq!(a["values"]["latency_ms"]["num"], 450.0);
    live.nothing().await;

    // The same reading again changes nothing worth sending.
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Warn, 450.0));
    live.nothing().await;
}

#[tokio::test]
async fn a_burst_of_changes_is_one_event() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["latency_ms"])))]),
    )
    .await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Ok, 1.0));
    let mut live = open_live(&bob, "en").await;
    live.event().await;
    tokio::time::pause();
    for n in 2..8 {
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(w.a, snap(Status::Ok, f64::from(n)));
        tokio::task::yield_now().await;
    }
    let update = live.event().await;
    assert_eq!(
        update["services"][w.a.to_string()]["values"]["latency_ms"]["num"],
        7.0
    );
    live.nothing().await;
}

#[tokio::test]
async fn an_unbound_or_hidden_service_never_makes_an_event() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    let hidden = service(&w.app, "Router", Visibility::Admin, &[]).await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["status"])))]),
    )
    .await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Ok, 1.0));
    let mut live = open_live(&bob, "en").await;
    live.event().await;
    tokio::time::pause();

    // Visible to Bob but not on his page; and not visible to him at all.
    for id in [w.c, hidden] {
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(id, snap(Status::Bad, 5.0));
        live.nothing().await;
    }
    // Removal of one he has no card for says nothing either.
    w.app.magpie.poller.remove_for_tests(hidden);
    live.nothing().await;
}

#[tokio::test]
async fn hiding_a_service_while_the_stream_is_open_ends_its_events_with_a_removal() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status"]))),
                card("c2", Some(bind(w.b, &["status"]))),
            ],
        ),
    )
    .await;
    for id in [w.a, w.b] {
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(id, snap(Status::Ok, 1.0));
    }
    let mut live = open_live(&bob, "en").await;
    assert_eq!(ids(&live.event().await).len(), 2);
    tokio::time::pause();

    set_visibility(&w.app, w.b, "Wiki", Visibility::Admin, &[]).await;
    let update = live.event().await;
    assert_eq!(update["removed"], json!([w.b]));
    assert_eq!(update["services"], json!({}));
    assert!(!update.to_string().contains("Wiki"));

    // Its snapshots do not reach Bob any more; Blog's still do.
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.b, snap(Status::Bad, 9.0));
    live.nothing().await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Warn, 2.0));
    let update = live.event().await;
    assert_eq!(ids(&update), vec![w.a.to_string()]);

    // Made visible to him again, it comes back whole.
    set_visibility(&w.app, w.b, "Wiki", Visibility::All, &[]).await;
    let update = live.event().await;
    assert_eq!(ids(&update), vec![w.b.to_string()]);
    assert_eq!(update["services"][w.b.to_string()]["status"], "bad");
}

#[tokio::test]
async fn a_new_name_is_sent_without_a_new_reading() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["status"])))]),
    )
    .await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Ok, 1.0));
    let mut live = open_live(&bob, "en").await;
    live.event().await;
    tokio::time::pause();

    store::update(
        &w.app.magpie.core,
        w.a,
        &input("Blog (new)", Visibility::All, &[]),
    )
    .await
    .unwrap();
    w.app
        .magpie
        .service_events
        .send(ServiceEvent::Updated(w.a))
        .unwrap();
    let update = live.event().await;
    assert_eq!(update["services"][w.a.to_string()]["name"], "Blog (new)");
}

#[tokio::test]
async fn a_deleted_service_is_removed() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["status"])))]),
    )
    .await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Ok, 1.0));
    let mut live = open_live(&bob, "en").await;
    live.event().await;
    tokio::time::pause();

    store::delete(&w.app.magpie.core, w.a).await.unwrap();
    w.app.magpie.poller.remove_for_tests(w.a);
    let update = live.event().await;
    assert_eq!(update["removed"], json!([w.a]));
    assert_eq!(update["services"], json!({}));
}

#[tokio::test]
async fn a_receiver_that_fell_behind_gets_the_whole_document_again() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status"]))),
                card("c2", Some(bind(w.b, &["status"]))),
            ],
        ),
    )
    .await;
    for id in [w.a, w.b] {
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(id, snap(Status::Ok, 1.0));
    }
    let mut live = open_live(&bob, "en").await;
    live.event().await;
    // Let the stream's own task settle on the channel.
    tokio::task::yield_now().await;
    tokio::time::pause();

    // The poller's channel holds 256: this overflows it before anything can
    // read (the test runs on one thread and never yields in the loop).
    for n in 0..400 {
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(w.a, snap(Status::Ok, f64::from(n % 7)));
    }
    let update = live.event().await;
    assert_eq!(update["full"], true, "{update}");
    // B did not change, and is in it all the same.
    assert_eq!(ids(&update), sorted([w.a, w.b]));
    live.nothing().await;
}

#[tokio::test]
async fn the_error_label_follows_the_request_language_inside_the_stream() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["status"])))]),
    )
    .await;
    let mut bad = snap(Status::Bad, 0.0);
    bad.error = Some("timeout".into());
    w.app.magpie.poller.set_snapshot_for_tests(w.a, bad.clone());
    let mut live = open_live(&bob, "nb").await;
    let first = live.event().await;
    assert_eq!(
        first["services"][w.a.to_string()]["error_label"],
        "Tidsavbrudd"
    );
    tokio::time::pause();
    bad.error = Some("connection failed".into());
    w.app.magpie.poller.set_snapshot_for_tests(w.a, bad);
    let update = live.event().await;
    assert_eq!(
        update["services"][w.a.to_string()]["error_label"],
        "Fikk ikke kontakt"
    );
}

#[tokio::test]
async fn the_stream_ends_when_the_server_shuts_down() {
    let w = world().await;
    let bob = Web::new(&w.app, "bob", "viewer").await;
    let mut live = open_live(&bob, "en").await;
    live.event().await;
    tokio::task::yield_now().await;
    w.app.magpie.begin_shutdown();
    live.ended().await;
}

#[tokio::test]
async fn the_stream_ends_when_the_session_is_revoked_or_the_user_goes() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["status"])))]),
    )
    .await;
    let mut live = open_live(&bob, "en").await;
    live.event().await;
    tokio::time::pause();
    w.app
        .exec(
            "UPDATE users SET session_version = session_version + 1 WHERE username = 'bob'".into(),
        )
        .await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Ok, 1.0));
    live.ended().await;
}

#[tokio::test]
async fn a_role_change_is_seen_by_a_stream_that_is_open() {
    // Ada is an admin and sees the admin-only service; once she is a viewer
    // the open stream stops carrying it.
    let w = world().await;
    let mut ada = Web::new(&w.app, "ada", "admin").await;
    let _root = Web::new(&w.app, "root", "admin").await;
    let secret = service(&w.app, "Router", Visibility::Admin, &[]).await;
    save(
        &mut ada,
        &doc(0, vec![card("c1", Some(bind(secret, &["status"])))]),
    )
    .await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(secret, snap(Status::Ok, 1.0));
    let mut live = open_live(&ada, "en").await;
    assert_eq!(ids(&live.event().await), vec![secret.to_string()]);
    tokio::time::pause();
    w.app
        .exec("UPDATE users SET role = 'viewer' WHERE username = 'ada'".into())
        .await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(secret, snap(Status::Bad, 1.0));
    let update = live.event().await;
    assert_eq!(update["removed"], json!([secret]));
    assert!(!update.to_string().contains("Router"));
}

// ---------------------------------------------------------------------------
// Whose services a token shows

/// The token the extension's connect flow gives `web`'s user: consent, then
/// the code swapped for a token (read access plus the `start:owner` scope).
async fn extension_token(app: &App, web: &mut Web<'_>) -> String {
    const REDIRECT: &str =
        "https://9bd6a096502a3a8555d679b3f64704219dffb501.extensions.allizom.org/";
    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let res = web
        .post(
            "/connect/extension",
            &[
                ("current_password", common::PASSWORD),
                ("redirect_uri", REDIRECT),
                ("state", "c3RhdGUtZnJvbS10aGUtZXh0ZW5zaW9u"),
                ("code_challenge", CHALLENGE),
                ("code_challenge_method", "S256"),
                ("device", "Test"),
                ("decision", "approve"),
            ],
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let location = url::Url::parse(res.header("location")).unwrap();
    let code = location
        .query_pairs()
        .find(|(k, _)| k == "code")
        .unwrap()
        .1
        .into_owned();
    let res = common::post_json(
        app,
        "/api/v1/extension/token",
        &json!({ "code": code, "code_verifier": VERIFIER }).to_string(),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    res.json()["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn an_admins_plain_read_token_gets_no_admin_only_values_but_the_extensions_token_does() {
    let w = world().await;
    let mut ada = Web::new(&w.app, "ada", "admin").await;
    let ada_id = user_id(&w.app, "ada").await;
    let secret = service(&w.app, "Admins-Only-Svc", Visibility::Admin, &[]).await;
    save(
        &mut ada,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status"]))),
                card("c2", Some(bind(secret, &["status"]))),
            ],
        ),
    )
    .await;
    for id in [w.a, secret] {
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(id, snap(Status::Ok, 1.0));
    }
    let path = "/api/v1/me/values";

    // The session's own page has both.
    assert_eq!(ids(&embed(&mut ada).await), sorted([w.a, secret]));

    // A script's read token (no product scope) is a viewer's: no admin-only name.
    let plain = make_token(&w.app, ada_id, TokenScope::Read, "script").await;
    let res = bearer_get(&w.app, path, &plain).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(ids(&res.json()), sorted([w.a]));
    assert!(!res.body.contains("Admins-Only-Svc"), "{}", res.body);

    // The token the connect flow gives: the admin's own view.
    let ext = extension_token(&w.app, &mut ada).await;
    let res = bearer_get(&w.app, path, &ext).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(ids(&res.json()), sorted([w.a, secret]));
    assert_eq!(
        res.json()["services"][secret.to_string()]["name"],
        "Admins-Only-Svc"
    );
    assert_clean(&res.body, "the extension's values");

    // A read-and-write token is the account's own role, as everywhere in the kit.
    let write = make_token(&w.app, ada_id, TokenScope::Write, "writer").await;
    let res = bearer_get(&w.app, path, &write).await;
    assert_eq!(ids(&res.json()), sorted([w.a, secret]));
}

#[tokio::test]
async fn a_viewers_extension_token_never_gets_admin_only_values() {
    let w = world().await;
    let _ada = Web::new(&w.app, "ada", "admin").await;
    let mut vera = Web::new(&w.app, "vera", "viewer").await;
    let vera_id = user_id(&w.app, "vera").await;
    let was_open = service(&w.app, "Was-Open-Svc", Visibility::All, &[]).await;
    save(
        &mut vera,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status"]))),
                card("c2", Some(bind(was_open, &["status"]))),
            ],
        ),
    )
    .await;
    for id in [w.a, was_open] {
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(id, snap(Status::Ok, 1.0));
    }
    // The binding stays in her layout (a save keeps an old binding), the service is hidden.
    set_visibility(&w.app, was_open, "Was-Open-Svc", Visibility::Admin, &[]).await;

    let ext = extension_token(&w.app, &mut vera).await;
    let res = bearer_get(&w.app, "/api/v1/me/values", &ext).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(ids(&res.json()), sorted([w.a]));
    assert!(!res.body.contains("Was-Open-Svc"), "{}", res.body);

    // Giving a plain token the scope by hand gains nothing: the owner's role decides.
    let plain = make_token(&w.app, vera_id, TokenScope::Read, "mine").await;
    let id: i64 = w
        .app
        .magpie
        .core
        .with_conn(|c| {
            c.query_row("SELECT id FROM api_tokens WHERE name = 'mine'", [], |r| {
                r.get(0)
            })
        })
        .await
        .unwrap();
    w.app
        .magpie
        .core
        .with_conn(move |c| vole_kit::db::api_tokens::add_scope(c, id, "start:owner"))
        .await
        .unwrap();
    let res = bearer_get(&w.app, "/api/v1/me/values", &plain).await;
    assert_eq!(ids(&res.json()), sorted([w.a]));
    assert!(!res.body.contains("Was-Open-Svc"));
}

#[tokio::test]
async fn no_kind_of_token_opens_the_stream() {
    let w = world().await;
    let mut ada = Web::new(&w.app, "ada", "admin").await;
    let ada_id = user_id(&w.app, "ada").await;
    let ext = extension_token(&w.app, &mut ada).await;
    let read = make_token(&w.app, ada_id, TokenScope::Read, "r").await;
    let write = make_token(&w.app, ada_id, TokenScope::Write, "w").await;
    for token in [ext, read, write] {
        let res = w
            .app
            .send(
                Request::get("/start/live")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_ne!(res.status, StatusCode::OK, "{}", res.body);
        assert_ne!(res.header("content-type"), "text/event-stream");
    }
}

// ---------------------------------------------------------------------------
// Views of services that become visible, and bindings that go

fn add_ids(event: &Value) -> Vec<i64> {
    let mut ids: Vec<i64> = event["services_add"]
        .as_array()
        .expect("services_add")
        .iter()
        .map(|v| v["id"].as_i64().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

fn sorted_ids(ids: &[i64]) -> Vec<i64> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids
}

#[tokio::test]
async fn the_first_event_carries_a_view_of_each_entry_and_later_ones_only_new_services() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status"]))),
                card("c2", Some(bind(w.b, &["status"]))),
            ],
        ),
    )
    .await;
    for id in [w.a, w.b] {
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(id, snap(Status::Ok, 1.0));
    }
    let mut live = open_live(&bob, "nb").await;
    let first = live.event().await;
    // Every service Bob may see, the unbound Printer too: the editor's picker
    // offers it.
    assert_eq!(add_ids(&first), sorted_ids(&[w.a, w.b, w.c]));
    // The view has what #mm-services has: name, kind, interval, labelled metrics.
    let view = first["services_add"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == w.a)
        .unwrap();
    assert_eq!(view["name"], "Blog");
    assert_eq!(view["kind"], "http");
    assert_eq!(view["interval_s"], 60);
    assert_eq!(view["metrics"][0]["id"], "status");
    assert!(!view["metrics"][0]["label"].as_str().unwrap().is_empty());
    assert_eq!(view["metrics"][0]["format"], "number");
    assert!(view.get("url").is_none() && view.get("settings").is_none());
    assert_clean(&first.to_string(), "first event");
    tokio::time::pause();

    // A change of a reading brings no view: the page has them.
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Warn, 9.0));
    let update = live.event().await;
    assert_eq!(update["services_add"], json!([]));
}

#[tokio::test]
async fn a_service_that_is_hidden_and_shown_again_comes_back_with_its_view() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status"]))),
                card("c2", Some(bind(w.b, &["status"]))),
            ],
        ),
    )
    .await;
    for id in [w.a, w.b] {
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(id, snap(Status::Ok, 1.0));
    }
    let mut live = open_live(&bob, "en").await;
    live.event().await;
    tokio::time::pause();

    set_visibility(&w.app, w.b, "Wiki", Visibility::Admin, &[]).await;
    let hidden = live.event().await;
    assert_eq!(hidden["removed"], json!([w.b]));
    assert_eq!(hidden["services_add"], json!([]));
    assert!(!hidden.to_string().contains("Wiki"));

    set_visibility(&w.app, w.b, "Wiki", Visibility::All, &[]).await;
    let shown = live.event().await;
    assert_eq!(add_ids(&shown), vec![w.b]);
    assert_eq!(shown["services_add"][0]["name"], "Wiki");
    assert!(shown["services"][w.b.to_string()].is_object());
    live.nothing().await;
}

#[tokio::test]
async fn a_service_hidden_at_load_arrives_with_its_view_once_it_is_visible() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    let bob_id = user_id(&w.app, "bob").await;
    let vera_id = common::add_user(&w.app, "vera", "viewer").await;
    save(
        &mut bob,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status"]))),
                card("c2", Some(bind(w.b, &["status"]))),
            ],
        ),
    )
    .await;
    // Wiki is for selected users, and the list leaves Bob out.
    set_visibility(&w.app, w.b, "Wiki", Visibility::Users, &[vera_id]).await;
    for id in [w.a, w.b] {
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(id, snap(Status::Ok, 1.0));
    }
    assert_eq!(ids(&embed(&mut bob).await), sorted([w.a]));
    let mut live = open_live(&bob, "en").await;
    let first = live.event().await;
    assert_eq!(ids(&first), sorted([w.a]));
    // The services Bob may see: Blog and the unbound Printer, not Wiki.
    assert_eq!(add_ids(&first), sorted_ids(&[w.a, w.c]));
    assert!(!first.to_string().contains("Wiki"));
    tokio::time::pause();

    // Its readings are nothing to him.
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.b, snap(Status::Bad, 5.0));
    live.nothing().await;

    // Put on the list, it arrives whole, with its view.
    set_visibility(&w.app, w.b, "Wiki", Visibility::Users, &[vera_id, bob_id]).await;
    let update = live.event().await;
    assert_eq!(ids(&update), sorted([w.b]));
    assert_eq!(add_ids(&update), vec![w.b]);
    assert_eq!(update["services_add"][0]["name"], "Wiki");
    assert_eq!(update["services"][w.b.to_string()]["status"], "bad");
}

#[tokio::test]
async fn unbinding_a_card_while_the_stream_is_open_removes_the_service_at_the_next_wake() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status"]))),
                card("c2", Some(bind(w.b, &["status"]))),
            ],
        ),
    )
    .await;
    for id in [w.a, w.b] {
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(id, snap(Status::Ok, 1.0));
    }
    let mut live = open_live(&bob, "en").await;
    assert_eq!(ids(&live.event().await).len(), 2);
    tokio::time::pause();

    // The card for Wiki loses its binding (a save from another tab).
    save(
        &mut bob,
        &doc(
            1,
            vec![card("c1", Some(bind(w.a, &["status"]))), card("c2", None)],
        ),
    )
    .await;
    // A save wakes nothing by itself; the next wake finds it.
    live.nothing().await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.c, snap(Status::Ok, 1.0));
    let update = live.event().await;
    assert_eq!(update["removed"], json!([w.b]));
    assert_eq!(update["services"], json!({}));
    assert_eq!(update["services_add"], json!([]));

    // Its readings say nothing more.
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.b, snap(Status::Bad, 9.0));
    live.nothing().await;
}

// ---------------------------------------------------------------------------
// Details (lists, bars and the like next to the numbers)

const DETAIL_MARK: &str = "Hidden-Detail-Mark-7731";

fn with_details(mut s: Snapshot, text: &str) -> Snapshot {
    s.details = vec![Block::List {
        title_key: "x".into(),
        items: vec![ListItem::new(text)],
    }];
    s
}

#[tokio::test]
async fn details_ride_in_the_entry_and_only_for_those_who_see_the_service() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    let bob_id = user_id(&w.app, "bob").await;
    save(
        &mut bob,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["status"]))),
                card("c2", Some(bind(w.b, &["status"]))),
            ],
        ),
    )
    .await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Ok, 1.0));
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.b, with_details(snap(Status::Ok, 2.0), DETAIL_MARK));

    // The embed: an entry with blocks has `details`, one without has no key.
    let values = embed(&mut bob).await;
    let b = &values["services"][w.b.to_string()];
    assert_eq!(
        b["details"],
        json!([{ "kind": "list", "title_key": "x", "items": [{ "text": DETAIL_MARK }] }])
    );
    assert!(values["services"][w.a.to_string()]["details"].is_null());

    // The token API says the same.
    let token = make_token(&w.app, bob_id, TokenScope::Read, "t").await;
    let res = bearer_get(&w.app, "/api/v1/me/values", &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        res.json()["services"][w.b.to_string()]["details"],
        b["details"]
    );

    // The stream's first event carries it too.
    let mut live = open_live(&bob, "en").await;
    let first = live.event().await;
    assert_eq!(first["services"][w.b.to_string()]["details"], b["details"]);
    tokio::time::pause();

    // Hidden from Bob: the details go with the entry, on every path.
    set_visibility(&w.app, w.b, "Wiki", Visibility::Admin, &[]).await;
    let update = live.event().await;
    assert_eq!(update["removed"], json!([w.b]));
    assert!(!update.to_string().contains(DETAIL_MARK), "{update}");
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.b, with_details(snap(Status::Bad, 9.0), DETAIL_MARK));
    live.nothing().await;
    let page = page(&mut bob, "en").await;
    assert!(
        !page.body.contains(DETAIL_MARK),
        "the page leaks the details"
    );
    let res = bearer_get(&w.app, "/api/v1/me/values", &token).await;
    assert!(!res.body.contains(DETAIL_MARK), "{}", res.body);
}

#[tokio::test]
async fn a_change_of_the_details_alone_is_an_event() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["status"])))]),
    )
    .await;
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, with_details(snap(Status::Ok, 1.0), "one"));
    let mut live = open_live(&bob, "en").await;
    live.event().await;
    tokio::time::pause();

    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, with_details(snap(Status::Ok, 1.0), "two"));
    let update = live.event().await;
    assert_eq!(
        update["services"][w.a.to_string()]["details"][0]["items"][0]["text"],
        "two"
    );
    // The same blocks again change nothing worth sending.
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, with_details(snap(Status::Ok, 1.0), "two"));
    live.nothing().await;
    // Blocks that go away are an event too, and the key goes with them.
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Ok, 1.0));
    let update = live.event().await;
    assert!(update["services"][w.a.to_string()]["details"].is_null());
}

// ---------------------------------------------------------------------------
// History: the curve of every bound number

fn now_s() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// A good reading of `latency` at `at`.
fn reading(at: i64, latency: f64) -> Snapshot {
    let mut s = snap(Status::Ok, latency);
    s.fetched_at = jiff::Timestamp::from_second(at).unwrap();
    s
}

/// What the poller does with a reading: its numbers go to the history (a
/// point per reading: interval 300 s) and the snapshot is published.
fn feed(app: &App, id: i64, at: i64, latency: f64) {
    let s = reading(at, latency);
    app.magpie.poller.record_history_for_tests(id, 300, &s);
    app.magpie.poller.set_snapshot_for_tests(id, s);
}

/// A day's rows in the table, as a reading at `at` leaves them.
async fn store_daily(app: &App, id: i64, at: i64, latency: f64) {
    let w = mindful_magpie::history::History::default().record(
        id,
        300,
        at,
        vec![("latency_ms".into(), latency), ("status".into(), 200.0)],
    );
    mindful_magpie::history::persist(&app.magpie.core, w)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_embed_and_the_token_api_carry_the_history_of_bound_metrics_only() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    let bob_id = user_id(&w.app, "bob").await;
    let now = now_s();
    // A service that bob binds and that is then hidden from him.
    let hidden = service(&w.app, "Router", Visibility::All, &[]).await;
    save(
        &mut bob,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["latency_ms"]))),
                card("c2", Some(bind(hidden, &["latency_ms"]))),
            ],
        ),
    )
    .await;
    for (n, id) in [w.a, w.c, hidden].into_iter().enumerate() {
        let latency = [12.0, 4242.5, 7777.5][n];
        feed(&w.app, id, now - 600, latency);
        feed(&w.app, id, now - 300, latency + 1.0);
        feed(&w.app, id, now, latency + 2.0);
    }
    store_daily(&w.app, w.a, now, 12.0).await;
    store_daily(&w.app, w.c, now, 4242.5).await;
    store_daily(&w.app, hidden, now, 7777.5).await;
    set_visibility(&w.app, hidden, "Router", Visibility::Admin, &[]).await;

    let page = embed(&mut bob).await;
    assert_eq!(ids(&page), vec![w.a.to_string()]);
    let a = &page["services"][w.a.to_string()];
    // Only the bound metric (not `status`), the last three points, oldest first.
    assert_eq!(
        a["history"],
        json!({ "latency_ms": [[now - 600, 12], [now - 300, 13], [now, 14]] })
    );
    // The same for the days, as [day, min, max, avg].
    let day = mindful_magpie::history::day_of(now);
    assert_eq!(
        a["history_daily"],
        json!({ "latency_ms": [[day, 12, 12, 12]] })
    );
    assert!(a.get("history_append").is_none());
    // Neither the unbound service nor the hidden one leaves a trace.
    let text = page.to_string();
    for stray in ["4242.5", "4243.5", "7777.5", "7778.5", "Printer", "Router"] {
        assert!(!text.contains(stray), "the embed leaks {stray}: {text}");
    }

    // The token API says the same.
    let token = make_token(&w.app, bob_id, TokenScope::Read, "t").await;
    let res = bearer_get(&w.app, "/api/v1/me/values", &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let mut api = res.json();
    let mut again = embed(&mut bob).await;
    api["now"] = json!(0);
    again["now"] = json!(0);
    assert_eq!(api, again);
    assert_eq!(
        api["services"][w.a.to_string()]["history"]["latency_ms"],
        json!([[now - 600, 12], [now - 300, 13], [now, 14]])
    );
    for stray in ["4242.5", "7777.5", "7778.5", "Router"] {
        assert!(!res.body.contains(stray), "the API leaks {stray}");
    }
}

#[tokio::test]
async fn a_bound_metric_with_no_history_yet_has_no_history_field() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["latency_ms"])))]),
    )
    .await;
    // A reading that was put in without being recorded.
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.a, snap(Status::Ok, 12.0));
    let page = embed(&mut bob).await;
    let a = &page["services"][w.a.to_string()];
    assert_eq!(a["status"], "ok");
    assert!(a.get("history").is_none(), "{a}");
    assert!(a.get("history_daily").is_none(), "{a}");
}

#[tokio::test]
async fn the_first_event_has_the_history_and_a_later_one_only_the_new_point() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.a, &["latency_ms"]))),
                card("c2", Some(bind(w.b, &["status"]))),
            ],
        ),
    )
    .await;
    let now = now_s();
    for id in [w.a, w.b] {
        feed(&w.app, id, now - 600, 10.0);
        feed(&w.app, id, now - 300, 11.0);
    }
    // Printer is not bound: its readings and history stay out of the stream.
    feed(&w.app, w.c, now - 300, 4242.5);
    store_daily(&w.app, w.a, now - 300, 10.0).await;

    let mut live = open_live(&bob, "en").await;
    let first = live.event().await;
    assert_eq!(first["full"], true);
    let a = &first["services"][w.a.to_string()];
    assert_eq!(
        a["history"],
        json!({ "latency_ms": [[now - 600, 10], [now - 300, 11]] })
    );
    assert_eq!(
        a["history_daily"]["latency_ms"].as_array().unwrap().len(),
        1
    );
    assert!(a.get("history_append").is_none());
    // B is bound on `status`: its history is the status's.
    assert_eq!(
        first["services"][w.b.to_string()]["history"],
        json!({ "status": [[now - 600, 200], [now - 300, 200]] })
    );
    assert!(!first.to_string().contains("4242.5"));
    tokio::time::pause();

    // A new reading: the entry has the point to add and no history.
    feed(&w.app, w.a, now, 12.5);
    let update = live.event().await;
    assert_eq!(update["full"], false);
    assert_eq!(ids(&update), vec![w.a.to_string()]);
    let a = &update["services"][w.a.to_string()];
    assert_eq!(a["history_append"], json!({ "latency_ms": [now, 12.5] }));
    assert!(a.get("history").is_none(), "{a}");
    assert!(a.get("history_daily").is_none(), "{a}");
    live.nothing().await;

    // The same reading again adds nothing; the unbound service says nothing.
    feed(&w.app, w.a, now, 12.5);
    feed(&w.app, w.c, now, 4243.5);
    live.nothing().await;
}

#[tokio::test]
async fn several_new_points_between_two_events_arrive_as_the_whole_series() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["latency_ms"])))]),
    )
    .await;
    let now = now_s();
    feed(&w.app, w.a, now - 900, 10.0);
    let mut live = open_live(&bob, "en").await;
    live.event().await;
    tokio::task::yield_now().await;
    tokio::time::pause();

    // Two readings inside one quiet period: one event, and the client has
    // missed a point, so it gets the series, not an append.
    feed(&w.app, w.a, now - 600, 11.0);
    feed(&w.app, w.a, now - 300, 12.0);
    let update = live.event().await;
    assert_eq!(update["full"], false);
    let a = &update["services"][w.a.to_string()];
    assert_eq!(
        a["history"],
        json!({ "latency_ms": [[now - 900, 10], [now - 600, 11], [now - 300, 12]] })
    );
    assert!(a.get("history_append").is_none(), "{a}");
    live.nothing().await;
}

#[tokio::test]
async fn a_receiver_that_fell_behind_gets_the_history_again() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["latency_ms"])))]),
    )
    .await;
    let now = now_s();
    feed(&w.app, w.a, now - 3600, 1.0);
    let mut live = open_live(&bob, "en").await;
    live.event().await;
    tokio::task::yield_now().await;
    tokio::time::pause();

    // 400 readings overflow the poller's channel (256) before the stream reads.
    for n in 0..400 {
        feed(&w.app, w.a, now - 3000 + n * 7, 2.0 + (n % 5) as f64);
    }
    let update = live.event().await;
    assert_eq!(update["full"], true, "{update}");
    let a = &update["services"][w.a.to_string()];
    let points = a["history"]["latency_ms"].as_array().unwrap();
    assert!(a.get("history_append").is_none(), "{a}");
    // The whole series the poller holds, not a part of it.
    let held = w
        .app
        .magpie
        .poller
        .history(w.a, &["latency_ms".to_string()].into(), now_s());
    assert_eq!(points.len(), held["latency_ms"].len());
    assert!(points.len() > 100, "{}", points.len());
    live.nothing().await;
}

#[tokio::test]
async fn one_service_with_eight_metrics_stays_near_40_kib_of_history() {
    use mindful_magpie::layout::Layout;
    use mindful_magpie::services::Viewer;
    use mindful_magpie::values::values_for;

    let w = world().await;
    let now = now_s();
    let names: Vec<String> = (1..=8).map(|k| format!("m{k}")).collect();
    // A full day at 5 minutes: 288 points of every metric, with numbers of
    // the length a latency or a load has (four significant digits).
    for i in 0..288i64 {
        let mut s = Snapshot::new(Status::Ok);
        s.fetched_at = jiff::Timestamp::from_second(now - (287 - i) * 300).unwrap();
        for (k, name) in names.iter().enumerate() {
            let v = ((i * 37 + k as i64 * 101) % 9000) as f64 / 10.0;
            s.values.insert(name.clone(), Reading::num(v));
        }
        w.app.magpie.poller.record_history_for_tests(w.a, 300, &s);
    }
    let layout: Layout = serde_json::from_value(doc(
        0,
        vec![card("c1", Some(json!({ "id": w.a, "metrics": names })))],
    ))
    .unwrap();
    let doc = values_for(
        &w.app.magpie.core,
        &w.app.magpie.poller,
        Viewer::new(1, true),
        &layout,
    )
    .await
    .unwrap();
    let history = &doc.services[&w.a.to_string()].history;
    assert_eq!(history.len(), 8);
    assert!(history.values().all(|points| points.len() == 288));
    let size = serde_json::to_string(history).unwrap().len();
    assert!(
        size < 44 * 1024,
        "{size} bytes of history for one service with 8 metrics"
    );
}

// ---------------------------------------------------------------------------
// History on a stream that stays open

/// Moves the poller's clock so that it reads 12:00 UTC (a test that moves it
/// on by minutes or days then never runs into midnight by accident); the
/// clock as the poller has it.
fn noon(app: &App) -> i64 {
    let now = app.magpie.poller.now();
    app.magpie
        .poller
        .advance_clock_for_tests((43_200 - now.rem_euclid(86_400)).rem_euclid(86_400));
    app.magpie.poller.now()
}

/// [`feed`] for a service polled every minute: its points are 5-minute slots.
fn feed_fast(app: &App, id: i64, at: i64, latency: f64) {
    let s = reading(at, latency);
    app.magpie.poller.record_history_for_tests(id, 60, &s);
    app.magpie.poller.set_snapshot_for_tests(id, s);
}

#[tokio::test]
async fn a_new_utc_day_sends_the_history_and_the_daily_rows_again() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["latency_ms"])))]),
    )
    .await;
    // The poller's clock reads 23:55 UTC; the stream opens with two points.
    let now = w.app.magpie.poller.now();
    w.app
        .magpie
        .poller
        .advance_clock_for_tests((86_400 - 300 - now.rem_euclid(86_400)).rem_euclid(86_400));
    let now = w.app.magpie.poller.now();
    feed(&w.app, w.a, now - 600, 10.0);
    feed(&w.app, w.a, now - 300, 11.0);
    store_daily(&w.app, w.a, now - 300, 10.0).await;
    let mut live = open_live(&bob, "en").await;
    let first = live.event().await;
    assert_eq!(first["full"], true);
    tokio::task::yield_now().await;
    tokio::time::pause();

    // Ten minutes on, past midnight: a quarter of an hour has not passed and
    // the page's points are all still within 24 hours, so a point alone would
    // be an append with no daily rows. The new day sends the series and both
    // days' rows instead.
    w.app.magpie.poller.advance_clock_for_tests(600);
    let later = w.app.magpie.poller.now();
    assert_ne!(
        mindful_magpie::history::day_of(later),
        mindful_magpie::history::day_of(now)
    );
    store_daily(&w.app, w.a, later, 20.0).await;
    feed(&w.app, w.a, later, 20.0);
    let update = live.event().await;
    assert_eq!(update["full"], false);
    let a = &update["services"][w.a.to_string()];
    assert_eq!(
        a["history"],
        json!({ "latency_ms": [[now - 600, 10], [now - 300, 11], [later, 20]] }),
        "{a}"
    );
    assert!(a.get("history_append").is_none(), "{a}");
    assert_eq!(
        a["history_daily"]["latency_ms"].as_array().unwrap().len(),
        2
    );
    live.nothing().await;
}

#[tokio::test]
async fn the_daily_rows_are_sent_again_every_15_minutes_at_most() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["latency_ms"])))]),
    )
    .await;
    let mut now = noon(&w.app);
    feed(&w.app, w.a, now - 300, 10.0);
    store_daily(&w.app, w.a, now - 300, 10.0).await;
    let mut live = open_live(&bob, "en").await;
    live.event().await;
    tokio::task::yield_now().await;
    tokio::time::pause();

    // A minute on: a point, no daily rows.
    now += 60;
    w.app.magpie.poller.advance_clock_for_tests(60);
    feed(&w.app, w.a, now, 11.0);
    let a = live.event().await["services"][w.a.to_string()].clone();
    assert_eq!(a["history_append"]["latency_ms"][1], 11, "{a}");
    assert!(a.get("history_daily").is_none(), "{a}");

    // A quarter of an hour after the first: the point and today's row (which
    // has grown) as a delta, not the series.
    now += 900;
    w.app.magpie.poller.advance_clock_for_tests(900);
    store_daily(&w.app, w.a, now, 30.0).await;
    feed(&w.app, w.a, now, 12.0);
    let a = live.event().await["services"][w.a.to_string()].clone();
    assert_eq!(a["history_append"]["latency_ms"][1], 12, "{a}");
    assert!(a.get("history").is_none(), "{a}");
    let day = &a["history_daily"]["latency_ms"];
    assert_eq!(day.as_array().unwrap().len(), 1, "{a}");
    assert_eq!(day[0][2], 30, "the highest of today: {a}");

    // And not again a minute later.
    now += 60;
    w.app.magpie.poller.advance_clock_for_tests(60);
    feed(&w.app, w.a, now, 13.0);
    let a = live.event().await["services"][w.a.to_string()].clone();
    assert!(a.get("history_daily").is_none(), "{a}");
}

#[tokio::test]
async fn a_rewritten_slot_and_a_new_slot_between_two_events_send_the_series() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["latency_ms"])))]),
    )
    .await;
    let now = now_s();
    let slot = now - now % 300;
    feed_fast(&w.app, w.a, slot + 5, 10.0);
    let mut live = open_live(&bob, "en").await;
    let first = live.event().await;
    assert_eq!(
        first["services"][w.a.to_string()]["history"],
        json!({ "latency_ms": [[slot, 10]] })
    );
    tokio::task::yield_now().await;
    tokio::time::pause();

    // The slot the client has is rewritten, and the next slot starts, before
    // the stream looks: an append of the new slot alone would leave the
    // client with the old value of the first.
    feed_fast(&w.app, w.a, slot + 20, 11.0);
    feed_fast(&w.app, w.a, slot + 305, 12.0);
    let update = live.event().await;
    let a = &update["services"][w.a.to_string()];
    assert_eq!(
        a["history"],
        json!({ "latency_ms": [[slot, 11], [slot + 300, 12]] }),
        "{a}"
    );
    assert!(a.get("history_append").is_none(), "{a}");
    live.nothing().await;

    // A rewrite of the newest slot alone is an append with the slot's time.
    feed_fast(&w.app, w.a, slot + 330, 13.0);
    let update = live.event().await;
    assert_eq!(
        update["services"][w.a.to_string()]["history_append"],
        json!({ "latency_ms": [slot + 300, 13] })
    );
    assert!(update["services"][w.a.to_string()].get("history").is_none());
}

#[tokio::test]
async fn a_metric_bound_while_the_stream_is_open_arrives_as_a_whole_series() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(0, vec![card("c1", Some(bind(w.a, &["latency_ms"])))]),
    )
    .await;
    let now = now_s();
    feed(&w.app, w.a, now - 600, 10.0);
    feed(&w.app, w.a, now - 300, 11.0);
    let mut live = open_live(&bob, "en").await;
    let first = live.event().await;
    let a = &first["services"][w.a.to_string()];
    assert_eq!(a["history"].as_object().unwrap().len(), 1);
    tokio::task::yield_now().await;
    tokio::time::pause();

    // The card gets a second number (a save from another tab); the next
    // wake has its whole series, and the first one's new point only.
    save(
        &mut bob,
        &doc(
            1,
            vec![card("c1", Some(bind(w.a, &["latency_ms", "status"])))],
        ),
    )
    .await;
    feed(&w.app, w.a, now, 12.0);
    let update = live.event().await;
    let a = &update["services"][w.a.to_string()];
    assert_eq!(
        a["history"],
        json!({ "status": [[now - 600, 200], [now - 300, 200], [now, 200]] }),
        "{a}"
    );
    assert_eq!(
        a["history_append"],
        json!({ "latency_ms": [now, 12] }),
        "{a}"
    );
}

#[tokio::test]
async fn a_metric_unbound_while_the_stream_is_open_gets_no_more_points() {
    let w = world().await;
    let mut bob = Web::new(&w.app, "bob", "viewer").await;
    save(
        &mut bob,
        &doc(
            0,
            vec![card("c1", Some(bind(w.a, &["latency_ms", "status"])))],
        ),
    )
    .await;
    let now = now_s();
    feed(&w.app, w.a, now - 600, 10.0);
    let mut live = open_live(&bob, "en").await;
    let first = live.event().await;
    assert_eq!(
        first["services"][w.a.to_string()]["history"]
            .as_object()
            .unwrap()
            .len(),
        2
    );
    tokio::task::yield_now().await;
    tokio::time::pause();

    save(
        &mut bob,
        &doc(1, vec![card("c1", Some(bind(w.a, &["latency_ms"])))]),
    )
    .await;
    feed(&w.app, w.a, now - 300, 11.0);
    let update = live.event().await;
    let a = &update["services"][w.a.to_string()];
    assert_eq!(
        a["history_append"],
        json!({ "latency_ms": [now - 300, 11] }),
        "{a}"
    );
    assert!(a.get("history").is_none(), "{a}");

    // Bound again: the whole series of it comes back, with what it gained.
    save(
        &mut bob,
        &doc(
            2,
            vec![card("c1", Some(bind(w.a, &["latency_ms", "status"])))],
        ),
    )
    .await;
    feed(&w.app, w.a, now, 12.0);
    let update = live.event().await;
    let a = &update["services"][w.a.to_string()];
    assert_eq!(
        a["history"],
        json!({ "status": [[now - 600, 200], [now - 300, 200], [now, 200]] }),
        "{a}"
    );
}
