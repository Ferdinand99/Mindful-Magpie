//! Personal services: a service that belongs to one user (the `github` kind).
//! Only its user sees, changes, tests and uses it, on every path the page and
//! the extension have: the stores, the pages of «My integrations», the admin's
//! pages (which answer 404), the card binding, the values, the needs, the
//! event stream, the acknowledgements and the picker. Also what deleting the
//! user does, the migration, and the poller with a stub of GitHub.

mod common;

use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{ConnectInfo, Request as AxumRequest, State};
use axum::http::{HeaderValue, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use common::{App, HOST, Res, Web, add_user, bearer_get, make_token, user_id};
use futures_util::{Stream, StreamExt as _};
use mindful_magpie::integrations::github::GithubKind;
use mindful_magpie::integrations::poller::{CONFIG_KEY, Options, Poller};
use mindful_magpie::integrations::{
    self, Integration, Level, Need, Snapshot, Status, Value as Reading,
};
use mindful_magpie::services::{
    Scope, SecretChange, ServiceEvent, ServiceInput, Viewer, Visibility, store,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;
use vole_kit::auth::TokenScope;

const TOKEN: &str = "github_pat_11PERS0NAL-Tok3n-1a2b3c4d5e6f";
const PURPOSE: &str = "magpie.service.secret";

// ---------------------------------------------------------------------------
// Helpers

fn github(name: &str) -> ServiceInput {
    ServiceInput {
        kind: "github".into(),
        name: name.into(),
        url: None,
        settings: json!({}),
        secret: SecretChange::Replace(TOKEN.into()),
        interval_s: 300,
        visibility: Visibility::Users,
        users: Vec::new(),
    }
}

fn shared(name: &str) -> ServiceInput {
    ServiceInput {
        kind: "http".into(),
        name: name.into(),
        url: Some("http://10.55.66.77:8123/personal-check".into()),
        settings: json!({}),
        secret: SecretChange::Keep,
        interval_s: 60,
        visibility: Visibility::All,
        users: Vec::new(),
    }
}

async fn personal_for(app: &App, user: i64, name: &str) -> i64 {
    store::create_in(&app.magpie.core, Scope::Personal(user), &github(name))
        .await
        .unwrap()
}

async fn shared_service(app: &App, name: &str) -> i64 {
    store::create(&app.magpie.core, &shared(name))
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
    json!({ "id": id, "metrics": ["review_requests", "ci_red"] })
}

async fn save(web: &mut Web<'_>, layout: &Value) -> Res {
    let req = Request::post("/start/layout")
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header("hx-request", "true")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(layout).unwrap()))
        .unwrap();
    web.send(req).await
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

/// `GET path` as a browser that asks for English (without a language the
/// pages show their translation keys in these tests).
async fn en(web: &mut Web<'_>, path: &str) -> Res {
    let req = Request::get(path)
        .header("accept-language", "en")
        .body(Body::empty())
        .unwrap();
    web.send(req).await
}

async fn page(web: &mut Web<'_>) -> String {
    let res = en(web, "/").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    res.body
}

fn need(key: &str, level: Level, label: &str, count: u32) -> Need {
    Need {
        key: key.into(),
        level,
        label_key: label.into(),
        params: [("count".to_string(), count.to_string())].into(),
        score: f64::from(count),
        since: None,
    }
}

fn snapshot(needs: Vec<Need>) -> Snapshot {
    let mut s = Snapshot::new(Status::Warn);
    s.fetched_at = jiff::Timestamp::from_second(1_760_000_000).unwrap();
    s.values.insert("review_requests".into(), Reading::num(3.0));
    s.values.insert("ci_red".into(), Reading::num(0.0));
    s.needs = needs;
    s
}

fn review_snapshot() -> Snapshot {
    snapshot(vec![need("gh:review", Level::Warn, "need.gh_review", 3)])
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

fn ids_of(map: &Value) -> Vec<i64> {
    let mut ids: Vec<i64> = map
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.parse().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

/// A list of plain ids (`services_visible`).
fn int_ids(list: &Value) -> Vec<i64> {
    let mut ids: Vec<i64> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

/// A list of service views (`#mm-services`, `services` of the start API).
fn view_ids(list: &Value) -> Vec<i64> {
    let mut ids: Vec<i64> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_i64().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

// --- the event stream ------------------------------------------------------

struct Live {
    body: Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>,
    buf: String,
}

async fn open_live(web: &Web<'_>) -> Live {
    let mut req = Request::get("/start/live")
        .header(header::HOST, HOST)
        .body(Body::empty())
        .unwrap();
    req.headers_mut().insert(
        header::COOKIE,
        HeaderValue::from_str(&web.cookie_header()).unwrap(),
    );
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:40009".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let res = web.app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    Live {
        body: Box::pin(res.into_body().into_data_stream()),
        buf: String::new(),
    }
}

impl Live {
    /// The next `values` event, or `None` when nothing came within `wait`.
    async fn next(&mut self, wait: Duration) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + wait;
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
                    return Some(serde_json::from_str(&data).unwrap());
                }
            }
            match tokio::time::timeout_at(deadline, self.body.next()).await {
                Err(_) | Ok(None) => return None,
                Ok(Some(chunk)) => self.buf.push_str(&String::from_utf8_lossy(&chunk.unwrap())),
            }
        }
    }
}

/// An admin, Alice with a personal service, Bob and Cy; one shared service.
struct World {
    app: App,
    alice: i64,
    /// Alice's personal service.
    mine: i64,
    /// A shared service for everybody.
    blog: i64,
}

async fn world() -> World {
    let app = App::new().await;
    add_user(&app, "ada", "admin").await;
    let alice = add_user(&app, "alice", "viewer").await;
    add_user(&app, "bob", "viewer").await;
    let mine = personal_for(&app, alice, "Alice GitHub").await;
    let blog = shared_service(&app, "Blog").await;
    app.magpie
        .poller
        .set_snapshot_for_tests(mine, review_snapshot());
    World {
        app,
        alice,
        mine,
        blog,
    }
}

impl World {
    /// The ids of the personal service and the shared one, in order.
    fn both(&self) -> Vec<i64> {
        let mut ids = vec![self.blog, self.mine];
        ids.sort_unstable();
        ids
    }

    async fn login(&self, name: &'static str) -> Web<'_> {
        let mut web = Web::anonymous(&self.app);
        let res = web
            .post(
                "/login",
                &[("username", name), ("password", common::PASSWORD)],
            )
            .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
        web
    }
}

// ---------------------------------------------------------------------------
// The store: who sees a personal service

#[tokio::test]
async fn a_personal_service_is_visible_to_its_user_and_nobody_else() {
    let w = world().await;
    let core = &w.app.magpie.core;
    let ada = user_id(&w.app, "ada").await;
    let bob = user_id(&w.app, "bob").await;

    let names = |list: Vec<mindful_magpie::services::ServiceSummary>| {
        list.into_iter().map(|s| s.name).collect::<Vec<_>>()
    };
    let alice = Viewer::new(w.alice, false);
    assert_eq!(
        names(store::visible_to(core, alice).await.unwrap()),
        ["Alice GitHub", "Blog"]
    );
    // Not Bob, and not an admin, who sees every shared service.
    assert_eq!(
        names(
            store::visible_to(core, Viewer::new(bob, false))
                .await
                .unwrap()
        ),
        ["Blog"]
    );
    assert_eq!(
        names(
            store::visible_to(core, Viewer::new(ada, true))
                .await
                .unwrap()
        ),
        ["Blog"]
    );
    // Not even a viewer value that claims to be Alice but is an admin of
    // somebody else: the service follows the user id.
    assert!(store::can_see(core, alice, w.mine).await.unwrap());
    assert!(
        !store::can_see(core, Viewer::new(bob, false), w.mine)
            .await
            .unwrap()
    );
    assert!(
        !store::can_see(core, Viewer::new(ada, true), w.mine)
            .await
            .unwrap()
    );
    // Alice as an admin still sees only her own personal ones.
    assert!(
        store::can_see(core, Viewer::new(w.alice, true), w.mine)
            .await
            .unwrap()
    );
    // The summary says it is personal, to the one who sees it.
    let summaries = store::visible_to(core, alice).await.unwrap();
    assert!(summaries.iter().find(|s| s.id == w.mine).unwrap().personal);
    assert!(!summaries.iter().find(|s| s.id == w.blog).unwrap().personal);

    // The scoped reads agree.
    assert_eq!(store::list(core).await.unwrap().len(), 1);
    assert_eq!(
        store::list_in(core, Scope::Personal(w.alice))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store::list_in(core, Scope::Personal(bob))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store::list_all(core).await.unwrap().len(), 2);
    assert!(store::get(core, w.mine).await.unwrap().is_none());
    assert!(
        store::get_in(core, Scope::Personal(bob), w.mine)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store::get_in(core, Scope::Personal(w.alice), w.blog)
            .await
            .unwrap()
            .is_none()
    );
    let found = store::get_in(core, Scope::Personal(w.alice), w.mine)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.owner_user_id, Some(w.alice));
    assert_eq!(found.url, None);
    assert_eq!(found.visibility, Visibility::Users);
    assert!(found.users.is_empty());
    assert!(store::get_any(core, w.mine).await.unwrap().is_some());

    // The secret opens only for its own scope.
    assert_eq!(
        store::open_secret_in(core, Some(Scope::Personal(w.alice)), w.mine)
            .await
            .unwrap()
            .as_deref(),
        Some(TOKEN)
    );
    for scope in [Scope::Shared, Scope::Personal(bob)] {
        assert_eq!(
            store::open_secret_in(core, Some(scope), w.mine)
                .await
                .unwrap(),
            None
        );
    }
    assert_eq!(store::open_secret(core, w.mine).await.unwrap(), None);
}

#[tokio::test]
async fn the_scope_decides_which_kinds_may_be_stored_and_who_may_change_them() {
    let w = world().await;
    let core = &w.app.magpie.core;
    let bob = user_id(&w.app, "bob").await;
    // A personal kind is not a shared service, and a shared kind is not a
    // personal one.
    assert!(store::create(core, &github("Shared GitHub")).await.is_err());
    assert!(
        store::create_in(core, Scope::Personal(bob), &shared("My site"))
            .await
            .is_err()
    );
    // A personal service has no address of its own.
    let mut with_url = github("Odd");
    with_url.url = Some("http://127.0.0.1:1/".into());
    assert!(
        store::create_in(core, Scope::Personal(bob), &with_url)
            .await
            .is_err()
    );
    // Whatever visibility the input names, nobody else sees it.
    let mut open = github("Open");
    open.visibility = Visibility::All;
    open.users = vec![w.alice];
    let id = store::create_in(core, Scope::Personal(bob), &open)
        .await
        .unwrap();
    let stored = store::get_in(core, Scope::Personal(bob), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.visibility, Visibility::Users);
    assert!(stored.users.is_empty());
    assert!(
        !store::visible_to(core, Viewer::new(w.alice, false))
            .await
            .unwrap()
            .iter()
            .any(|s| s.id == id)
    );

    // Another user, the admin's shared scope and the other kind of service
    // cannot change or delete it.
    let mut changed = github("Taken over");
    changed.secret = SecretChange::Clear;
    for scope in [Scope::Personal(bob), Scope::Shared] {
        let err = store::update_in(core, scope, w.mine, &changed).await;
        assert!(err.is_err(), "{scope:?}");
        assert!(
            !store::delete_in(core, scope, w.mine).await.unwrap(),
            "{scope:?}"
        );
    }
    assert!(store::update(core, w.blog, &shared("Blog")).await.is_ok());
    assert!(
        !store::delete_in(core, Scope::Personal(w.alice), w.blog)
            .await
            .unwrap()
    );
    let still = store::get_in(core, Scope::Personal(w.alice), w.mine)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still.name, "Alice GitHub");
    assert!(still.has_secret);

    // Its user changes it, and keeps the secret by leaving it alone.
    let mut renamed = github("Renamed");
    renamed.secret = SecretChange::Keep;
    store::update_in(core, Scope::Personal(w.alice), w.mine, &renamed)
        .await
        .unwrap();
    let after = store::get_in(core, Scope::Personal(w.alice), w.mine)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.name, "Renamed");
    assert!(after.has_secret);
    assert_eq!(after.owner_user_id, Some(w.alice));
}

#[tokio::test]
async fn the_token_is_sealed_with_the_service_purpose_and_never_stored_plain() {
    let w = world().await;
    let id = w.mine;
    let sealed: String = w
        .app
        .magpie
        .core
        .with_conn(move |c| {
            c.query_row("SELECT secret FROM services WHERE id = ?1", [id], |r| {
                r.get(0)
            })
        })
        .await
        .unwrap();
    assert!(!sealed.contains(TOKEN));
    let sealer = vole_kit::crypto::Sealer::new(&w.app.magpie.core.session_key, PURPOSE);
    assert_eq!(sealer.open(&sealed).unwrap(), TOKEN);
    // The database holds the token nowhere else.
    let dump = w
        .app
        .magpie
        .core
        .with_conn(|c| {
            let mut texts = Vec::new();
            for table in ["services", "service_snapshots", "acks", "service_users"] {
                let mut stmt = c.prepare(&format!("SELECT * FROM {table}"))?;
                let n = stmt.column_count();
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    for i in 0..n {
                        if let Ok(text) = row.get::<_, String>(i) {
                            texts.push(text);
                        }
                    }
                }
            }
            Ok(texts)
        })
        .await
        .unwrap();
    let plain = dump.iter().filter(|t| t.contains(TOKEN)).count();
    assert_eq!(plain, 0);
}

// ---------------------------------------------------------------------------
// The pages

#[tokio::test]
async fn the_admin_pages_answer_404_for_a_personal_service() {
    let w = world().await;
    let mut ada = w.login("ada").await;
    let missing = ada.get("/settings/services/987654").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    // Listed: the shared one, not the personal one.
    let list = ada.get("/settings/services").await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(list.body.contains("Blog"));
    assert!(!list.body.contains("Alice GitHub"), "{}", list.body);

    // Opened, saved, deleted, tested: a plain 404, like a missing one.
    let url = format!("/settings/services/{}", w.mine);
    let res = ada.get(&url).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.body, missing.body);
    let form = [
        ("kind", "github"),
        ("name", "Taken over"),
        ("interval", "300"),
        ("visibility", "all"),
        ("secret", "github_pat_admin"),
    ];
    assert_eq!(ada.post(&url, &form).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        ada.post(&format!("{url}/delete"), &[]).await.status,
        StatusCode::NOT_FOUND
    );
    // «Test» with this service's number finds no stored key.
    let id = w.mine.to_string();
    let res = ada
        .post_with(
            "/settings/services/test",
            &[
                ("kind", "http"),
                ("url", "http://127.0.0.1:1/"),
                ("id", &id),
            ],
            true,
        )
        .await;
    assert!(!res.body.contains(TOKEN));

    // The admin can't make a shared GitHub service either.
    let before = w.app.count("SELECT COUNT(*) FROM services").await;
    let res = ada
        .post(
            "/settings/services/new",
            &[
                ("kind", "github"),
                ("name", "Shared GitHub"),
                ("interval", "300"),
                ("visibility", "all"),
                ("secret", "github_pat_admin"),
            ],
        )
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(w.app.count("SELECT COUNT(*) FROM services").await, before);
    let res = ada.get("/settings/services/new?kind=github").await;
    assert!(
        !res.body.contains("github_pat_"),
        "no GitHub form in the admin's pages"
    );

    // Nothing of it changed.
    let name: String = w
        .app
        .magpie
        .core
        .with_conn(move |c| {
            c.query_row("SELECT name FROM services WHERE id = ?1", [w.mine], |r| {
                r.get(0)
            })
        })
        .await
        .unwrap();
    assert_eq!(name, "Alice GitHub");
}

#[tokio::test]
async fn my_integrations_is_for_every_signed_in_user_and_shows_only_their_own() {
    let w = world().await;
    let bob_id = user_id(&w.app, "bob").await;
    personal_for(&w.app, bob_id, "Bob GitHub").await;
    for (name, own, others) in [
        ("alice", "Alice GitHub", ["Bob GitHub", ""]),
        ("bob", "Bob GitHub", ["Alice GitHub", ""]),
        // An admin has personal services of their own and sees no one else's.
        ("ada", "", ["Alice GitHub", "Bob GitHub"]),
    ] {
        let mut web = w.login(name).await;
        let res = en(&mut web, "/settings/integrations").await;
        assert_eq!(res.status, StatusCode::OK, "{name}");
        assert!(res.body.contains("My integrations"), "{name}");
        if !own.is_empty() {
            assert!(res.body.contains(own), "{name}: {}", res.body);
        }
        for other in others.iter().filter(|o| !o.is_empty()) {
            assert!(!res.body.contains(other), "{name} sees {other}");
        }
        // The tab is in the settings navigation.
        assert!(res.body.contains("/settings/integrations"), "{name}");
    }
    // Not signed in: no page.
    let anonymous = Web::anonymous(&w.app).get("/settings/integrations").await;
    assert!(anonymous.status != StatusCode::OK);
}

#[tokio::test]
async fn another_users_personal_service_is_a_404_on_every_integrations_route() {
    let w = world().await;
    for name in ["bob", "ada"] {
        let mut web = w.login(name).await;
        let url = format!("/settings/integrations/{}", w.mine);
        let missing = web.get("/settings/integrations/987654").await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        let res = web.get(&url).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{name}");
        assert_eq!(res.body, missing.body, "{name}");
        let form = [
            ("kind", "github"),
            ("name", "Taken over"),
            ("interval", "300"),
            ("secret", "github_pat_other"),
        ];
        assert_eq!(
            web.post(&url, &form).await.status,
            StatusCode::NOT_FOUND,
            "{name}"
        );
        assert_eq!(
            web.post(&format!("{url}/delete"), &[]).await.status,
            StatusCode::NOT_FOUND,
            "{name}"
        );
        // «Test» with her service's number never uses her key.
        let id = w.mine.to_string();
        let res = web
            .post_with(
                "/settings/integrations/test",
                &[("kind", "github"), ("id", &id)],
                true,
            )
            .await;
        assert!(
            res.body.contains("data-test-status=\"unknown\""),
            "{}",
            res.body
        );
        assert!(!res.body.contains(TOKEN));
    }
    let row: (String, bool) = w
        .app
        .magpie
        .core
        .with_conn(move |c| {
            c.query_row(
                "SELECT name, secret IS NOT NULL FROM services WHERE id = ?1",
                [w.mine],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .await
        .unwrap();
    assert_eq!(row, ("Alice GitHub".to_string(), true));
}

#[tokio::test]
async fn a_user_adds_edits_tests_and_deletes_their_own_service() {
    let app = App::new().await;
    add_user(&app, "ada", "admin").await;
    // A viewer: no admin, no editor.
    let mut vera = Web::new(&app, "vera", "viewer").await;
    let vera_id = user_id(&app, "vera").await;

    let res = en(&mut vera, "/settings/integrations").await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.body.contains("No integrations yet"), "{}", res.body);

    // One kind: the form opens at once. It has no address and no visibility,
    // lists the permissions and says that one token covers one owner.
    let res = en(&mut vera, "/settings/integrations/new").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("name=\"secret\""));
    assert!(res.body.contains("name=\"f_owners\""));
    assert!(!res.body.contains("name=\"url\""), "no address to enter");
    assert!(!res.body.contains("name=\"visibility\""));
    for permission in [
        "Metadata",
        "Pull requests",
        "Commit statuses",
        "Actions",
        "Dependabot alerts",
        "Contents",
    ] {
        assert!(res.body.contains(permission), "{permission}");
    }
    assert!(res.body.contains("one owner or organisation"));
    assert!(res.body.contains("/settings/integrations/test"));

    // The same-origin proof is needed, as on the admin's pages.
    let form = [
        ("kind", "github"),
        ("name", "Work GitHub"),
        ("f_owners", "my-org"),
        ("interval", ""),
        ("secret", TOKEN),
        // None of these has any effect on a personal service.
        ("url", "http://169.254.169.254/"),
        ("visibility", "all"),
        ("users", "1"),
    ];
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form)
        .finish();
    for origin in [None, Some("http://evil.example")] {
        let mut req = Request::post("/settings/integrations/new")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some(origin) = origin {
            req = req.header(header::ORIGIN, origin);
        }
        let res = vera.send(req.body(Body::from(body.clone())).unwrap()).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{origin:?}");
    }
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 0);

    let res = vera.post("/settings/integrations/new", &form).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.header("location"), "/settings/integrations");
    let row = app
        .magpie
        .core
        .with_conn(|c| {
            c.query_row(
                "SELECT id, kind, name, url, settings, interval_s, visibility, owner_user_id,
                        (SELECT COUNT(*) FROM service_users)
                 FROM services",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, Option<i64>>(7)?,
                        r.get::<_, i64>(8)?,
                    ))
                },
            )
        })
        .await
        .unwrap();
    let id = row.0;
    assert_eq!(row.1, "github");
    assert_eq!(row.2, "Work GitHub");
    assert_eq!(row.3, None, "the address is fixed in the kind");
    assert_eq!(row.4, r#"{"owners":"my-org"}"#);
    assert_eq!(row.5, 300, "the kind's default interval");
    assert_eq!(row.6, "users");
    assert_eq!(row.7, Some(vera_id));
    assert_eq!(row.8, 0, "no viewers");
    // Sealed, and not shown again.
    let sealed: String = app
        .magpie
        .core
        .with_conn(move |c| {
            c.query_row("SELECT secret FROM services WHERE id = ?1", [id], |r| {
                r.get(0)
            })
        })
        .await
        .unwrap();
    let sealer = vole_kit::crypto::Sealer::new(&app.magpie.core.session_key, PURPOSE);
    assert_eq!(sealer.open(&sealed).unwrap(), TOKEN);

    let list = en(&mut vera, "/settings/integrations").await;
    assert!(list.body.contains("Work GitHub"));
    assert!(!list.body.contains(TOKEN));
    let edit = en(&mut vera, &format!("/settings/integrations/{id}")).await;
    assert_eq!(edit.status, StatusCode::OK);
    assert!(edit.body.contains("my-org"));
    assert!(!edit.body.contains(TOKEN));
    assert!(!edit.body.contains(&sealed));
    assert!(edit.body.contains("Saved – type to replace"));
    assert!(
        edit.body
            .contains(&format!("/settings/integrations/{id}/delete"))
    );

    // An edit without a key keeps the stored one.
    let url = format!("/settings/integrations/{id}");
    let res = vera
        .post(
            &url,
            &[
                ("kind", "github"),
                ("name", "Renamed"),
                ("f_owners", "my-org, other"),
                ("interval", "600"),
                ("secret", ""),
            ],
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let (name, settings, interval, same): (String, String, i64, String) = app
        .magpie
        .core
        .with_conn(move |c| {
            c.query_row(
                "SELECT name, settings, interval_s, secret FROM services WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
        })
        .await
        .unwrap();
    assert_eq!((name.as_str(), interval), ("Renamed", 600));
    assert_eq!(settings, r#"{"owners":"my-org, other"}"#);
    assert_eq!(same, sealed, "the key was kept");

    // Refused: a name, an interval below the kind's minimum, owners that are
    // not logins. The form comes back with what was typed and the reasons.
    for (field, form) in [
        (
            "interval",
            vec![("name", "x"), ("interval", "60"), ("f_owners", "")],
        ),
        (
            "owners",
            vec![("name", "x"), ("interval", "300"), ("f_owners", "a/b")],
        ),
        (
            "name",
            vec![("name", ""), ("interval", "300"), ("f_owners", "")],
        ),
    ] {
        let mut full = vec![("kind", "github"), ("secret", "")];
        full.extend(form);
        let res = vera.post(&url, &full).await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        assert!(res.body.contains("field-error"), "{field}: {}", res.body);
        assert!(!res.body.contains(TOKEN));
    }
    let unchanged: String = app
        .magpie
        .core
        .with_conn(move |c| {
            c.query_row("SELECT name FROM services WHERE id = ?1", [id], |r| {
                r.get(0)
            })
        })
        .await
        .unwrap();
    assert_eq!(unchanged, "Renamed");

    // A key typed anew replaces it; one that can't go in a header is refused.
    let res = vera
        .post(
            &url,
            &[
                ("kind", "github"),
                ("name", "Renamed"),
                ("interval", "600"),
                ("secret", "bad key with spaces"),
            ],
        )
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    let res = vera
        .post(
            &url,
            &[
                ("kind", "github"),
                ("name", "Renamed"),
                ("interval", "600"),
                ("secret", "github_pat_replaced"),
            ],
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(
        store::open_secret_in(&app.magpie.core, Some(Scope::Personal(vera_id)), id)
            .await
            .unwrap()
            .as_deref(),
        Some("github_pat_replaced")
    );

    // The other kinds are not for this page.
    let res = vera
        .post(
            "/settings/integrations/new",
            &[
                ("kind", "http"),
                ("name", "Site"),
                ("url", "http://127.0.0.1/"),
                ("interval", "60"),
            ],
        )
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 1);
    assert!(
        !vera
            .get("/settings/integrations/new?kind=http")
            .await
            .body
            .contains("name=\"url\"")
    );

    // «Test»: nothing is sent when there is nothing to send with, and the
    // form's address is never used.
    let res = vera
        .post_with(
            "/settings/integrations/test",
            &[
                ("kind", "github"),
                ("f_owners", ""),
                ("url", "http://127.0.0.1:1/"),
                ("secret", ""),
            ],
            true,
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body.contains("data-test-status=\"unknown\""),
        "{}",
        res.body
    );
    let res = vera
        .post_with(
            "/settings/integrations/test",
            &[("kind", "github"), ("f_owners", "a/b"), ("secret", TOKEN)],
            true,
        )
        .await;
    assert!(
        res.body.contains("data-test-status=\"unknown\""),
        "{}",
        res.body
    );
    assert!(!res.body.contains(TOKEN));
    let res = vera
        .post_with(
            "/settings/integrations/test",
            &[("kind", "http"), ("url", "http://127.0.0.1:1/")],
            true,
        )
        .await;
    assert!(
        res.body.contains("data-test-status=\"unknown\""),
        "{}",
        res.body
    );
    // «Test» is behind the same-origin proof too.
    let req = Request::post("/settings/integrations/test")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("kind=github"))
        .unwrap();
    assert_eq!(vera.send(req).await.status, StatusCode::FORBIDDEN);

    // Delete.
    let req = Request::post(format!("{url}/delete"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        vera.send(req).await.status,
        StatusCode::FORBIDDEN,
        "needs the proof"
    );
    let res = vera.post(&format!("{url}/delete"), &[]).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 0);
    assert_eq!(vera.get(&url).await.status, StatusCode::NOT_FOUND, "gone");
}

#[tokio::test]
async fn the_admin_has_a_my_integrations_page_of_their_own_too() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let ada_id = user_id(&app, "ada").await;
    let res = ada
        .post(
            "/settings/integrations/new",
            &[
                ("kind", "github"),
                ("name", "Ada GitHub"),
                ("interval", ""),
                ("secret", TOKEN),
            ],
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let owner: Option<i64> = app
        .magpie
        .core
        .with_conn(|c| c.query_row("SELECT owner_user_id FROM services", [], |r| r.get(0)))
        .await
        .unwrap();
    assert_eq!(owner, Some(ada_id));
    // Her own services page does not list it (that page is for shared ones).
    let list = ada.get("/settings/services").await;
    assert!(!list.body.contains("Ada GitHub"));
}

// ---------------------------------------------------------------------------
// Every path of the page and the extension

#[tokio::test]
async fn the_user_has_the_card_the_numbers_the_needs_the_stream_and_the_picker() {
    let w = world().await;
    let mut alice = w.login("alice").await;
    let res = save(&mut alice, &doc(0, vec![card("c1", Some(bind(w.mine)))])).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // The embed.
    let body = page(&mut alice).await;
    let values = block(&body, "mm-values");
    assert_eq!(ids_of(&values["services"]), vec![w.mine]);
    assert_eq!(
        values["services"][w.mine.to_string()]["name"],
        "Alice GitHub"
    );
    assert_eq!(
        values["services"][w.mine.to_string()]["values"]["review_requests"]["num"],
        3.0
    );
    let needs = values["needs"].as_array().unwrap();
    assert_eq!(needs.len(), 1);
    assert_eq!(needs[0]["label"], "Waiting for you: 3");
    assert_eq!(needs[0]["key"], "gh:review");
    // The picker: the service is offered, with its kind's metrics.
    let services = block(&body, "mm-services");
    assert_eq!(view_ids(&services), w.both());
    let mine = services
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == w.mine)
        .unwrap();
    assert_eq!(mine["kind"], "github");
    let metric_ids: Vec<&str> = mine["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        metric_ids,
        [
            "review_requests",
            "ci_red",
            "dependabot",
            "releases_24h",
            "open_prs"
        ]
    );
    // Never the address, the settings or the key.
    assert!(!body.contains(TOKEN));
    assert!(!body.contains("api.github.com"));

    // The API with her token.
    let token = make_token(&w.app, w.alice, TokenScope::Read, "ext").await;
    let res = bearer_get(&w.app, "/api/v1/me/values", &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(ids_of(&res.json()["services"]), vec![w.mine]);
    assert_eq!(res.json()["needs"].as_array().unwrap().len(), 1);
    let res = bearer_get(&w.app, "/api/v1/me/start", &token).await;
    assert_eq!(view_ids(&res.json()["services"]), w.both());

    // The stream.
    let mut live = open_live(&alice).await;
    let first = live.next(Duration::from_secs(10)).await.expect("an event");
    assert_eq!(ids_of(&first["services"]), vec![w.mine]);
    assert_eq!(int_ids(&first["services_visible"]), w.both());
    assert_eq!(first["needs"].as_array().unwrap().len(), 1);

    // The acknowledgement.
    let res = ack(&mut alice, w.mine, "gh:review").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.json()["needs"][0]["acked"], true);
    assert_eq!(w.app.count("SELECT COUNT(*) FROM acks").await, 1);
}

#[tokio::test]
async fn nobody_else_gets_it_by_any_path_not_even_an_admin() {
    let w = world().await;
    // Alice's layout is bound to it, as a real one would be.
    let mut alice = w.login("alice").await;
    save(&mut alice, &doc(0, vec![card("c1", Some(bind(w.mine)))])).await;
    drop(alice);

    let missing = w.mine + 1000;
    for name in ["bob", "ada"] {
        let mut web = w.login(name).await;
        let uid = user_id(&w.app, name).await;

        // The card binding: a 422 that is the one for a service that does not
        // exist, byte for byte.
        let for_mine = save(&mut web, &doc(0, vec![card("c1", Some(bind(w.mine)))])).await;
        let for_missing = save(&mut web, &doc(0, vec![card("c1", Some(bind(missing)))])).await;
        assert_eq!(
            for_mine.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{name}: {}",
            for_mine.body
        );
        assert_eq!(for_mine.status, for_missing.status, "{name}");
        assert_eq!(for_mine.body, for_missing.body, "{name}");
        assert!(!for_mine.body.contains("Alice") && !for_mine.body.contains("github"));
        // A metric that does not exist does not tell either.
        let probe = save(
            &mut web,
            &doc(
                0,
                vec![card(
                    "c1",
                    Some(json!({ "id": w.mine, "metrics": ["nope"] })),
                )],
            ),
        )
        .await;
        assert_eq!(probe.body, for_mine.body, "{name}");
        assert_eq!(
            w.app
                .count(&*Box::leak(
                    format!("SELECT COUNT(*) FROM layouts WHERE user_id = {uid}").into_boxed_str()
                ))
                .await,
            0,
            "{name}: nothing was saved"
        );

        // Even a layout that already has the binding (written some other way)
        // shows and counts nothing of it.
        let layout = doc(
            0,
            vec![
                card("c1", Some(bind(w.mine))),
                card("c2", Some(bind(w.blog))),
            ],
        );
        w.app
            .exec(format!(
                "INSERT INTO layouts (user_id, version, doc, updated_at)
                 VALUES ({uid}, 1, '{}', '2026-11-07T00:00:00Z')",
                layout.to_string().replace('\'', "''")
            ))
            .await;
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(w.blog, snapshot(vec![]));

        // The embed: the values, the needs and the picker.
        let body = page(&mut web).await;
        assert!(!body.contains("Alice GitHub"), "{name}: the page names it");
        assert!(!body.contains("gh:review"), "{name}");
        assert!(!body.contains("Waiting for you"), "{name}");
        let values = block(&body, "mm-values");
        assert_eq!(ids_of(&values["services"]), vec![w.blog], "{name}");
        assert!(values["needs"].as_array().unwrap().is_empty(), "{name}");
        assert_eq!(
            view_ids(&block(&body, "mm-services")),
            vec![w.blog],
            "{name}"
        );

        // The API, with a read token and with the extension's.
        let token = make_token(&w.app, uid, TokenScope::Read, "ext").await;
        let res = bearer_get(&w.app, "/api/v1/me/values", &token).await;
        assert_eq!(res.status, StatusCode::OK);
        assert_eq!(ids_of(&res.json()["services"]), vec![w.blog], "{name}");
        assert!(res.json()["needs"].as_array().unwrap().is_empty(), "{name}");
        assert!(!res.body.contains("Alice"), "{name}");
        let res = bearer_get(&w.app, "/api/v1/me/start", &token).await;
        assert_eq!(view_ids(&res.json()["services"]), vec![w.blog], "{name}");
        assert!(!res.body.contains("Alice"), "{name}");

        // The stream: not in the first event, and a new reading of it is no
        // news to this user.
        let mut live = open_live(&web).await;
        let first = live.next(Duration::from_secs(10)).await.expect("an event");
        assert_eq!(ids_of(&first["services"]), vec![w.blog], "{name}");
        assert_eq!(int_ids(&first["services_visible"]), vec![w.blog], "{name}");
        assert!(first["needs"].as_array().unwrap().is_empty(), "{name}");
        w.app
            .magpie
            .poller
            .set_snapshot_for_tests(w.mine, review_snapshot());
        if let Some(event) = live.next(Duration::from_secs(2)).await {
            let text = event.to_string();
            assert!(
                !text.contains("Alice") && !text.contains("gh:review"),
                "{name}: {text}"
            );
            assert!(
                event
                    .get("services")
                    .is_none_or(|s| !ids_of(s).contains(&w.mine)),
                "{name}: {text}"
            );
        }

        // The acknowledgement: a plain 404, like a service that is not there.
        let for_missing = ack(&mut web, missing, "gh:review").await;
        assert_eq!(for_missing.status, StatusCode::NOT_FOUND);
        let res = ack(&mut web, w.mine, "gh:review").await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{name}");
        assert_eq!(res.body, for_missing.body, "{name}");
    }
    assert_eq!(w.app.count("SELECT COUNT(*) FROM acks").await, 0);
}

#[tokio::test]
async fn an_admins_extension_token_does_not_get_it_either() {
    // The extension's token acts as an admin for admin-only services; a
    // personal service is not one of them.
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let alice = add_user(&app, "alice", "viewer").await;
    let mine = personal_for(&app, alice, "Alice GitHub").await;
    app.magpie
        .poller
        .set_snapshot_for_tests(mine, review_snapshot());
    save(&mut ada, &doc(0, vec![])).await;
    let admin_only = shared_service(&app, "Router").await;
    app.exec(format!(
        "UPDATE services SET visibility = 'admin' WHERE id = {admin_only}"
    ))
    .await;
    let ada_id = user_id(&app, "ada").await;
    let token = make_token(&app, ada_id, TokenScope::Read, "plain").await;
    let res = bearer_get(&app, "/api/v1/me/start", &token).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(!res.body.contains("Alice"));
    // The same through the store, for an admin viewer.
    let seen = store::visible_to(&app.magpie.core, Viewer::new(ada_id, true))
        .await
        .unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].name, "Router");
}

#[tokio::test]
async fn the_config_need_of_a_personal_service_is_for_its_user_not_an_admin() {
    let w = world().await;
    // The key is refused: «needs to be set up again» asks something of Alice.
    let config = |label: &str| Need {
        key: CONFIG_KEY.into(),
        level: Level::Bad,
        label_key: label.into(),
        params: Default::default(),
        score: 1.0,
        since: None,
    };
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.mine, snapshot(vec![config("need.service_config")]));
    w.app
        .magpie
        .poller
        .set_snapshot_for_tests(w.blog, snapshot(vec![config("need.service_config")]));
    let mut alice = w.login("alice").await;
    save(
        &mut alice,
        &doc(
            0,
            vec![
                card("c1", Some(bind(w.mine))),
                card("c2", Some(json!({ "id": w.blog, "metrics": ["status"] }))),
            ],
        ),
    )
    .await;
    let needs = block(&page(&mut alice).await, "mm-values")["needs"]
        .as_array()
        .unwrap()
        .clone();
    // Hers, not the shared one (which an admin has to set up again).
    assert_eq!(needs.len(), 1, "{needs:?}");
    assert_eq!(needs[0]["service_id"], w.mine);
    assert_eq!(needs[0]["key"], CONFIG_KEY);
    let res = ack(&mut alice, w.mine, CONFIG_KEY).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = ack(&mut alice, w.blog, CONFIG_KEY).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Deleting the user

#[tokio::test]
async fn deleting_the_user_deletes_their_personal_services_snapshots_and_acks() {
    let w = world().await;
    let bob_id = user_id(&w.app, "bob").await;
    let bobs = personal_for(&w.app, bob_id, "Bob GitHub").await;
    let mut alice = w.login("alice").await;
    save(&mut alice, &doc(0, vec![card("c1", Some(bind(w.mine)))])).await;
    assert_eq!(
        ack(&mut alice, w.mine, "gh:review").await.status,
        StatusCode::OK
    );
    // A stored reading (the poller writes them).
    for id in [w.mine, bobs] {
        w.app
            .exec(format!(
                "INSERT INTO service_snapshots (service_id, json, fetched_at)
                 VALUES ({id}, '{{}}', '2026-11-07T00:00:00Z')"
            ))
            .await;
    }
    assert_eq!(w.app.count("SELECT COUNT(*) FROM services").await, 3);
    assert_eq!(
        w.app.count("SELECT COUNT(*) FROM service_snapshots").await,
        2
    );
    assert_eq!(w.app.count("SELECT COUNT(*) FROM acks").await, 1);

    w.app
        .exec(format!("DELETE FROM users WHERE id = {}", w.alice))
        .await;
    // Hers are gone, with their snapshot and their acks; Bob's and the shared
    // one stay.
    assert_eq!(w.app.count("SELECT COUNT(*) FROM services").await, 2);
    assert_eq!(
        w.app
            .count(&*Box::leak(
                format!("SELECT COUNT(*) FROM services WHERE id = {}", w.mine).into_boxed_str()
            ))
            .await,
        0
    );
    assert_eq!(
        w.app.count("SELECT COUNT(*) FROM service_snapshots").await,
        1
    );
    assert_eq!(w.app.count("SELECT COUNT(*) FROM acks").await, 0);
    assert!(
        store::get_in(&w.app.magpie.core, Scope::Personal(bob_id), bobs)
            .await
            .unwrap()
            .is_some()
    );
}

// ---------------------------------------------------------------------------
// The poller with a stub of GitHub

/// What the stub of GitHub answers: the login to the login query, one red
/// repository and two reviews waiting to the numbers query.
async fn github_stub(
    State(seen): State<std::sync::Arc<std::sync::Mutex<Vec<Option<String>>>>>,
    request: AxumRequest,
) -> Response {
    let auth = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    seen.lock().unwrap().push(auth);
    let bytes = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let text = body["query"].as_str().unwrap_or_default();
    let answer = if text.contains("query Magpie") {
        json!({ "data": {
            "viewer": { "repositories": { "nodes": [{
                "nameWithOwner": "me/api",
                "defaultBranchRef": { "name": "main",
                    "target": { "statusCheckRollup": { "state": "FAILURE" } } },
                "vulnerabilityAlerts": { "totalCount": 4 },
                "releases": { "nodes": [] },
            }] } },
            "reviews": { "issueCount": 2 },
            "authored": { "issueCount": 1 },
        } })
    } else {
        json!({ "data": { "viewer": { "login": "octocat" } } })
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        answer.to_string(),
    )
        .into_response()
}

static STUB_KIND: OnceLock<GithubKind> = OnceLock::new();

fn stub_kinds(kind: &str) -> Option<&'static dyn Integration> {
    match kind {
        "github" => STUB_KIND.get().map(|k| k as &dyn Integration),
        other => integrations::find(other),
    }
}

#[tokio::test]
async fn the_poller_fetches_a_personal_service_with_its_users_token() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let router = Router::new().fallback(github_stub).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    STUB_KIND.set(GithubKind::with_base(base)).ok();

    let w = world().await;
    let poller = Poller::start_with(
        w.app.magpie.core.clone(),
        integrations::build_client(Duration::from_secs(5)),
        &w.app.magpie.service_events,
        Options {
            kinds: stub_kinds,
            spread: Duration::ZERO,
        },
    );
    let snapshot = {
        let mut found = None;
        for _ in 0..100 {
            if let Some(s) = poller.snapshot(w.mine)
                && s.fetched_at > jiff::Timestamp::from_second(1_770_000_000).unwrap()
            {
                found = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        found.expect("the poller fetched the personal service")
    };
    assert_eq!(snapshot.error, None);
    assert_eq!(snapshot.values["review_requests"].num, Some(2.0));
    assert_eq!(snapshot.values["ci_red"].num, Some(1.0));
    assert_eq!(snapshot.values["dependabot"].num, Some(4.0));
    assert_eq!(snapshot.values["open_prs"].num, Some(1.0));
    assert_eq!(snapshot.status, Status::Bad);
    let keys: Vec<&str> = snapshot.needs.iter().map(|n| n.key.as_str()).collect();
    assert_eq!(keys, ["gh:ci_red", "gh:review"]);
    // The user's own token went out, as the opened secret, and nobody else's.
    let seen = seen.lock().unwrap().clone();
    assert!(!seen.is_empty());
    for auth in seen {
        assert_eq!(auth.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
    }
    // And the snapshot is stored for the restart.
    let stored = w
        .app
        .count(&*Box::leak(
            format!(
                "SELECT COUNT(*) FROM service_snapshots WHERE service_id = {}",
                w.mine
            )
            .into_boxed_str(),
        ))
        .await;
    assert_eq!(stored, 1);

    // The user is deleted: the poller, told to look again, forgets the
    // service and its reading.
    w.app
        .exec(format!("DELETE FROM users WHERE id = {}", w.alice))
        .await;
    poller.resync();
    let mut gone = false;
    for _ in 0..100 {
        if poller.snapshot(w.mine).is_none() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "the personal service's reading is dropped");
    let _ = ServiceEvent::Deleted(0);
}

// ---------------------------------------------------------------------------
// The migration

/// Migrations 0001 to 0008, as a database before personal services has them.
fn up_to_0008() -> Vec<(String, String)> {
    [
        ("0001_init.sql", include_str!("../migrations/0001_init.sql")),
        (
            "0002_layouts.sql",
            include_str!("../migrations/0002_layouts.sql"),
        ),
        (
            "0003_extension_codes.sql",
            include_str!("../migrations/0003_extension_codes.sql"),
        ),
        (
            "0004_backgrounds.sql",
            include_str!("../migrations/0004_backgrounds.sql"),
        ),
        (
            "0005_kept_background.sql",
            include_str!("../migrations/0005_kept_background.sql"),
        ),
        (
            "0006_services.sql",
            include_str!("../migrations/0006_services.sql"),
        ),
        (
            "0007_snapshots.sql",
            include_str!("../migrations/0007_snapshots.sql"),
        ),
        ("0008_acks.sql", include_str!("../migrations/0008_acks.sql")),
    ]
    .into_iter()
    .map(|(n, s)| (n.to_string(), s.to_string()))
    .collect()
}

fn nothing_after(
    _: &mut rusqlite::Connection,
) -> Result<(), vole_kit::db::migrate::MigrationError> {
    Ok(())
}

#[tokio::test]
async fn migration_0009_on_a_database_at_0008_keeps_every_row_and_adds_the_column() {
    mindful_magpie::install();
    let dir = std::env::temp_dir().join(format!("mindful-magpie-m0009-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(mindful_magpie::product::DB_FILE);
    {
        static AT_0008: vole_kit::db::migrate::MigrationSet = vole_kit::db::migrate::MigrationSet {
            core: vole_kit::db::migrate::CORE,
            adopt_core: None,
            migrations: up_to_0008,
            fk_off_marker: "-- mm:foreign_keys=off",
            baseline: None,
            after: nothing_after,
        };
        let mut conn = rusqlite::Connection::open(&file).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        let applied = vole_kit::db::migrate::apply_migrations_with(&mut conn, &AT_0008).unwrap();
        assert!(applied.iter().any(|n| n.contains("0008_acks")));
        assert!(!applied.iter().any(|n| n.contains("0009")));
        // The database as 0008 left it: no owner column.
        let columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('services') WHERE name = 'owner_user_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(columns, 0);
        conn.execute_batch(
            "INSERT INTO users (id, username, password_hash, role) VALUES (7, 'ada', 'x', 'admin');
             INSERT INTO services (id, kind, name, url, settings, secret, interval_s, visibility,
                                   created_at, updated_at)
             VALUES (3, 'http', 'Blog', 'http://10.1.2.3/', '{}', 'sealed', 60, 'all', 't', 't');
             INSERT INTO service_snapshots (service_id, json, fetched_at) VALUES (3, '{}', 't');
             INSERT INTO acks (user_id, service_id, need_key, level, score, acked_at)
             VALUES (7, 3, 'down', 'bad', 1, 't');",
        )
        .unwrap();
    }

    let magpie = mindful_magpie::start(&dir, None).await.unwrap();
    let core = magpie.core.clone();
    // Only the new migration ran, and the old rows are intact and shared.
    let (applied, owner_cols, owner, name, snaps, acks): (i64, i64, Option<i64>, String, i64, i64) = core
        .with_conn(|c| {
            Ok((
                c.query_row(
                    "SELECT COUNT(*) FROM schema_migrations WHERE name LIKE '%0009_personal%'",
                    [],
                    |r| r.get(0),
                )?,
                c.query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('services') WHERE name = 'owner_user_id'",
                    [],
                    |r| r.get(0),
                )?,
                c.query_row("SELECT owner_user_id FROM services WHERE id = 3", [], |r| r.get(0))?,
                c.query_row("SELECT name FROM services WHERE id = 3", [], |r| r.get(0))?,
                c.query_row("SELECT COUNT(*) FROM service_snapshots", [], |r| r.get(0))?,
                c.query_row("SELECT COUNT(*) FROM acks", [], |r| r.get(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!((applied, owner_cols), (1, 1));
    assert_eq!(owner, None, "an old service stays shared");
    assert_eq!(name, "Blog");
    assert_eq!((snaps, acks), (1, 1));
    let shared = store::list(&core).await.unwrap();
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0].owner_user_id, None);
    assert_eq!(
        store::visible_to(&core, Viewer::new(7, false))
            .await
            .unwrap()
            .len(),
        1
    );

    // And the new column works: a personal service of the user goes with them.
    let id = store::create_in(&core, Scope::Personal(7), &github("Mine"))
        .await
        .unwrap();
    assert!(
        store::get_in(&core, Scope::Personal(7), id)
            .await
            .unwrap()
            .is_some()
    );
    core.with_conn(|c| c.execute("DELETE FROM users WHERE id = 7", []))
        .await
        .unwrap();
    assert!(store::get_any(&core, id).await.unwrap().is_none());
    // The shared service stayed; its acknowledgement went with the user.
    assert!(store::get(&core, 3).await.unwrap().is_some());
    drop(core);
    drop(magpie);
    let _ = std::fs::remove_dir_all(&dir);
}
