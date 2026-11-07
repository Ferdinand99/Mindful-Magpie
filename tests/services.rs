//! Services (Plan 4, task 1): the admin pages, sealed secrets, «Test», who
//! sees what, and the built-in `http` kind against a stub server that runs in
//! the test process.

mod common;

use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;
use common::{App, Res, Web, add_user};
use mindful_magpie::integrations::{self, FetchCtx, Integration, Status};
use mindful_magpie::services::{
    SecretChange, ServiceEvent, ServiceInput, Viewer, Visibility, store,
};
use serde_json::json;

const SECRET: &str = "s3cr3t-Tok3n-9f8e7d6c";
const PURPOSE: &str = "magpie.service.secret";

// ---------------------------------------------------------------------------
// The stub server

async fn auth(Path(want): Path<String>, headers: HeaderMap) -> StatusCode {
    let sent = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if sent == Some(format!("Bearer {want}").as_str()) {
        StatusCode::OK
    } else {
        StatusCode::UNAUTHORIZED
    }
}

async fn hop(Path(n): Path<u32>) -> axum::response::Response {
    if n == 0 {
        "arrived".into_response()
    } else {
        Redirect::temporary(&format!("/hop/{}", n - 1)).into_response()
    }
}

/// Redirects to the address in `?u=`.
async fn to(
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    match q.get("u") {
        Some(u) => Redirect::temporary(u).into_response(),
        None => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(5)).await;
    "late"
}

/// Serves on an ephemeral loopback port; the base URL without a trailing slash.
async fn stub() -> String {
    let app = Router::new()
        .route("/ok", get(|| async { "hello" }))
        .route(
            "/fail",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "boom") }),
        )
        .route(
            "/teapot",
            get(|| async { (StatusCode::IM_A_TEAPOT, "tea") }),
        )
        .route("/auth/{want}", get(auth))
        .route("/slow", get(slow))
        .route("/big", get(|| async { vec![b'a'; 2 * 1024 * 1024] }))
        .route("/hop/{n}", get(hop))
        .route(
            "/meta",
            get(|| async { Redirect::temporary("http://169.254.169.254/latest/meta-data/") }),
        )
        .route("/to", get(to))
        .route("/loop", get(|| async { Redirect::temporary("/loop") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

// ---------------------------------------------------------------------------
// Helpers

fn http_kind() -> &'static dyn Integration {
    integrations::find("http").expect("the http kind is registered")
}

fn ctx(url: &str, secret: Option<&str>) -> FetchCtx {
    FetchCtx {
        http: integrations::client().clone(),
        settings: json!({ "url": url }),
        secret: secret.map(str::to_string),
    }
}

async fn fetch(url: &str, secret: Option<&str>) -> integrations::Snapshot {
    http_kind()
        .fetch(&ctx(url, secret))
        .await
        .expect("the http kind answers with a snapshot")
}

/// The form of a new `http` service as the page posts it.
fn form<'a>(name: &'a str, url: &'a str, secret: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("kind", "http"),
        ("name", name),
        ("url", url),
        ("f_expected_status", ""),
        ("interval", ""),
        ("visibility", "admin"),
        ("secret", secret),
    ]
}

async fn last_id(app: &App) -> i64 {
    app.count("SELECT COALESCE(MAX(id), 0) FROM services").await
}

/// Creates a service through the page; its id.
async fn create_via_page(web: &mut Web<'_>, name: &str, url: &str, secret: &str) -> i64 {
    let res = web
        .post("/settings/services/new", &form(name, url, secret))
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    last_id(web.app).await
}

async fn stored_secret(app: &App, id: i64) -> Option<String> {
    app.magpie
        .core
        .with_conn(move |c| {
            c.query_row("SELECT secret FROM services WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
        })
        .await
        .unwrap()
}

fn input(name: &str, visibility: Visibility, users: &[i64]) -> ServiceInput {
    ServiceInput {
        kind: "http".into(),
        name: name.into(),
        url: Some("http://10.0.0.5/".into()),
        settings: json!({}),
        secret: SecretChange::Keep,
        interval_s: 60,
        visibility,
        users: users.to_vec(),
    }
}

fn assert_clean(res: &Res, what: &str) {
    assert!(!res.body.contains(SECRET), "{what} leaks the secret");
    assert!(!res.body.contains("Bearer s3"), "{what} leaks the header");
}

/// A result or an error text: no mention of the header at all.
fn assert_no_header(res: &Res, what: &str) {
    assert_clean(res, what);
    for needle in ["Bearer", "Authorization"] {
        assert!(!res.body.contains(needle), "{what} mentions {needle:?}");
    }
}

// ---------------------------------------------------------------------------
// The admin pages

#[tokio::test]
async fn an_admin_creates_an_http_service_and_it_is_listed() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Blog", "http://10.0.0.5:8080/health", "").await;
    let list = admin.get("/settings/services").await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(list.body.contains("Blog"));
    assert!(list.body.contains("Website"), "the kind's label");
    assert!(list.body.contains(&format!("/settings/services/{id}")));
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 1);
    // Defaults: the kind's interval, and only admins see it.
    assert_eq!(app.count("SELECT interval_s FROM services").await, 60);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM services WHERE visibility = 'admin'")
            .await,
        1
    );
}

#[tokio::test]
async fn only_admins_reach_the_service_routes() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Hidden-Blog-Name", "http://10.0.0.5/", SECRET).await;
    for role in ["viewer", "editor"] {
        let name: &'static str = if role == "viewer" { "vera" } else { "eddie" };
        let mut web = Web::new(&app, name, role).await;
        for path in [
            "/settings/services".to_string(),
            "/settings/services/new".to_string(),
            format!("/settings/services/{id}"),
        ] {
            let res = web.get(&path).await;
            assert_eq!(res.status, StatusCode::FORBIDDEN, "{role} GET {path}");
            assert!(
                !res.body.contains("Hidden-Blog-Name"),
                "{role} GET {path} names the service"
            );
        }
        let edit = format!("/settings/services/{id}");
        let delete = format!("/settings/services/{id}/delete");
        let f = form("Hacked", "http://10.0.0.6/", "");
        for path in [
            "/settings/services/new",
            edit.as_str(),
            delete.as_str(),
            "/settings/services/test",
        ] {
            let res = web.post(path, &f).await;
            assert_eq!(res.status, StatusCode::FORBIDDEN, "{role} POST {path}");
            assert!(
                !res.body.contains("Hidden-Blog-Name"),
                "{role} POST {path} names the service"
            );
        }
    }
    // Nothing changed.
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 1);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM services WHERE name = 'Hidden-Blog-Name'")
            .await,
        1
    );
}

#[tokio::test]
async fn the_settings_navigation_links_the_page_for_admins_only() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let mut viewer = Web::new(&app, "vera", "viewer").await;
    let a = admin.get("/settings/security").await;
    assert!(a.body.contains("href=\"/settings/services\""));
    let v = viewer.get("/settings/security").await;
    assert_eq!(v.status, StatusCode::OK);
    assert!(!v.body.contains("href=\"/settings/services\""));
}

#[tokio::test]
async fn the_secret_is_sealed_in_the_database() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Blog", "http://10.0.0.5/", SECRET).await;
    let stored = stored_secret(&app, id).await.expect("a stored secret");
    assert!(stored.starts_with(vole_kit::crypto::PREFIX), "{stored}");
    assert!(vole_kit::crypto::is_sealed(&stored));
    assert_ne!(stored, SECRET);
    assert!(!stored.contains(SECRET));
    // It opens for the service purpose and for no other.
    let key = &app.magpie.core.session_key;
    assert_eq!(
        vole_kit::crypto::open(key, PURPOSE, &stored).as_deref(),
        Ok(SECRET)
    );
    assert!(vole_kit::crypto::open(key, "something.else", &stored).is_err());
    // Not anywhere else in the row either.
    let rest: String = app
        .magpie
        .core
        .with_conn(move |c| {
            c.query_row(
                "SELECT kind || name || COALESCE(url, '') || settings FROM services WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
        })
        .await
        .unwrap();
    assert!(!rest.contains(SECRET));
}

#[tokio::test]
async fn the_secret_never_comes_back_in_html() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Blog", "http://10.0.0.5/", SECRET).await;
    let edit = admin.get(&format!("/settings/services/{id}")).await;
    assert_eq!(edit.status, StatusCode::OK);
    assert_clean(&edit, "the edit page");
    assert!(
        edit.body.contains("Saved – type to replace"),
        "{}",
        edit.body
    );
    let list = admin.get("/settings/services").await;
    assert_clean(&list, "the list");
    // A refused form (no name) is shown again without the secret.
    let mut f = form("", "http://10.0.0.5/", SECRET);
    f[1].1 = "";
    let res = admin.post("/settings/services/new", &f).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_clean(&res, "a refused form");
}

#[tokio::test]
async fn an_empty_secret_keeps_the_stored_one() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Blog", "http://10.0.0.5/", SECRET).await;
    let before = stored_secret(&app, id).await;
    let res = admin
        .post(
            &format!("/settings/services/{id}"),
            &form("Blog renamed", "http://10.0.0.5/", ""),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(stored_secret(&app, id).await, before);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM services WHERE name = 'Blog renamed'")
            .await,
        1
    );
    // A new value replaces it.
    let res = admin
        .post(
            &format!("/settings/services/{id}"),
            &form("Blog renamed", "http://10.0.0.5/", "another-secret-value"),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let now = stored_secret(&app, id).await.unwrap();
    assert_ne!(Some(now.clone()), before);
    assert_eq!(
        vole_kit::crypto::open(&app.magpie.core.session_key, PURPOSE, &now).as_deref(),
        Ok("another-secret-value")
    );
    // And it can be taken away.
    let mut f = form("Blog renamed", "http://10.0.0.5/", "");
    f.push(("clear_secret", "true"));
    let res = admin.post(&format!("/settings/services/{id}"), &f).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(stored_secret(&app, id).await, None);
}

#[tokio::test]
async fn a_bad_form_is_refused_and_nothing_is_stored() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    // Credentials in the address, a non-http scheme, an interval below the
    // kind's minimum, an unknown kind, a bad status range.
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("userinfo", form("A", "http://user:pw-in-url@10.0.0.5/", "")),
        ("scheme", form("A", "ftp://10.0.0.5/", "")),
        ("no url", form("A", "", "")),
        ("name", form("   ", "http://10.0.0.5/", "")),
        (
            "secret with a line break",
            form("A", "http://10.0.0.5/", "a\r\nX-Evil: 1"),
        ),
        (
            "secret with a space",
            form("A", "http://10.0.0.5/", "two words"),
        ),
        (
            "secret with non-ASCII",
            form("A", "http://10.0.0.5/", "n\u{f8}kkel"),
        ),
        ("interval", {
            let mut f = form("A", "http://10.0.0.5/", "");
            f[4].1 = "1";
            f
        }),
        ("visibility", {
            let mut f = form("A", "http://10.0.0.5/", "");
            f[5].1 = "everyone";
            f
        }),
        ("kind", {
            let mut f = form("A", "http://10.0.0.5/", "");
            f[0].1 = "nope";
            f
        }),
        ("status range", {
            let mut f = form("A", "http://10.0.0.5/", "");
            f[3].1 = "abc";
            f
        }),
    ];
    for (what, f) in cases {
        let res = admin.post("/settings/services/new", &f).await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{what}");
        assert!(!res.body.contains("pw-in-url"), "{what} shows the userinfo");
    }
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 0);
}

#[tokio::test]
async fn unknown_ids_are_not_found() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    assert_eq!(
        admin.get("/settings/services/999").await.status,
        StatusCode::NOT_FOUND
    );
    let res = admin
        .post("/settings/services/999", &form("A", "http://10.0.0.5/", ""))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_a_service_removes_it_and_its_viewers() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let uid = add_user(&app, "vera", "viewer").await;
    let id = store::create(&app.magpie.core, &input("Blog", Visibility::Users, &[uid]))
        .await
        .unwrap();
    assert_eq!(app.count("SELECT COUNT(*) FROM service_users").await, 1);
    let res = admin
        .post(&format!("/settings/services/{id}/delete"), &[])
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 0);
    assert_eq!(app.count("SELECT COUNT(*) FROM service_users").await, 0);
}

// ---------------------------------------------------------------------------
// «Test»

#[tokio::test]
async fn test_shows_status_latency_and_never_saves() {
    let base = stub().await;
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let res = admin
        .post(
            "/settings/services/test",
            &form("Blog", &format!("{base}/ok"), SECRET),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("data-test-status=\"ok\""), "{}", res.body);
    assert!(res.body.contains("200"), "the HTTP status");
    assert!(res.body.contains("Response time"), "the latency metric");
    assert_no_header(&res, "the test result");
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 0);
}

#[tokio::test]
async fn test_reports_a_bad_answer() {
    let base = stub().await;
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let res = admin
        .post(
            "/settings/services/test",
            &form("Blog", &format!("{base}/fail"), SECRET),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body.contains("data-test-status=\"bad\""),
        "{}",
        res.body
    );
    assert!(res.body.contains("500"));
    assert_no_header(&res, "the test result");
}

#[tokio::test]
async fn test_uses_the_stored_secret_when_the_field_is_empty() {
    let base = stub().await;
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let url = format!("{base}/auth/{SECRET}");
    let id = create_via_page(&mut admin, "Guarded", &url, SECRET).await;
    // Empty field + the id: the stored secret is sent, so the stub says 200.
    let mut f = form("Guarded", &url, "");
    let id_text = id.to_string();
    f.push(("id", &id_text));
    let res = admin.post("/settings/services/test", &f).await;
    assert!(res.body.contains("data-test-status=\"ok\""), "{}", res.body);
    assert_no_header(&res, "the test result");
    // Without the id there is no secret to use: the stub says 401.
    let res = admin
        .post("/settings/services/test", &form("Guarded", &url, ""))
        .await;
    assert!(
        res.body.contains("data-test-status=\"bad\""),
        "{}",
        res.body
    );
    assert!(res.body.contains("401"));
    // A typed secret wins over the stored one.
    let res = admin
        .post("/settings/services/test", &{
            let mut f = form("Guarded", &url, "wrong-secret");
            f.push(("id", &id_text));
            f
        })
        .await;
    assert!(res.body.contains("data-test-status=\"bad\""));
    assert!(!res.body.contains("wrong-secret"));
}

#[tokio::test]
async fn test_refuses_the_metadata_address_without_leaking() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let res = admin
        .post(
            "/settings/services/test",
            &form("Cloud", "http://169.254.169.254/latest/meta-data/", SECRET),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains("data-test-status=\"bad\""),
        "{}",
        res.body
    );
    assert!(res.body.contains("not allowed"), "{}", res.body);
    assert_no_header(&res, "the refusal");
    assert!(
        !res.body.contains("169.254"),
        "the address is not echoed in the error"
    );
}

#[tokio::test]
async fn test_with_a_bad_form_says_what_is_wrong() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let res = admin
        .post(
            "/settings/services/test",
            &form("Blog", "http://user:pw-in-url@10.0.0.5/", SECRET),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        res.body.contains("data-test-status=\"unknown\""),
        "{}",
        res.body
    );
    assert!(!res.body.contains("pw-in-url"));
    assert_no_header(&res, "the test result");
}

// ---------------------------------------------------------------------------
// Who sees which service

#[tokio::test]
async fn visible_to_honours_all_admin_and_users() {
    let app = App::new().await;
    let core = &app.magpie.core;
    let admin = add_user(&app, "root", "admin").await;
    let ann = add_user(&app, "ann", "viewer").await;
    let bob = add_user(&app, "bob", "viewer").await;
    let all = store::create(core, &input("For all", Visibility::All, &[]))
        .await
        .unwrap();
    let only_admin = store::create(core, &input("Admins", Visibility::Admin, &[]))
        .await
        .unwrap();
    let ann_only = store::create(core, &input("Ann's", Visibility::Users, &[ann]))
        .await
        .unwrap();

    let names = |v: Vec<mindful_magpie::services::ServiceSummary>| {
        v.into_iter().map(|s| s.name).collect::<Vec<_>>()
    };
    let seen_by_admin = names(
        store::visible_to(core, Viewer::new(admin, true))
            .await
            .unwrap(),
    );
    assert_eq!(seen_by_admin, ["Admins", "Ann's", "For all"]);
    let seen_by_ann = names(
        store::visible_to(core, Viewer::new(ann, false))
            .await
            .unwrap(),
    );
    assert_eq!(seen_by_ann, ["Ann's", "For all"]);
    let seen_by_bob = names(
        store::visible_to(core, Viewer::new(bob, false))
            .await
            .unwrap(),
    );
    assert_eq!(seen_by_bob, ["For all"]);

    for (who, id, expected) in [
        (Viewer::new(ann, false), all, true),
        (Viewer::new(ann, false), only_admin, false),
        (Viewer::new(ann, false), ann_only, true),
        (Viewer::new(bob, false), ann_only, false),
        (Viewer::new(bob, false), only_admin, false),
        (Viewer::new(admin, true), only_admin, true),
        (Viewer::new(admin, true), ann_only, true),
        (Viewer::new(bob, false), 9999, false),
        (Viewer::new(admin, true), 9999, false),
    ] {
        assert_eq!(
            store::can_see(core, who, id).await.unwrap(),
            expected,
            "{who:?} {id}"
        );
    }
}

#[tokio::test]
async fn deleting_a_user_removes_their_service_rows() {
    let app = App::new().await;
    let core = &app.magpie.core;
    let ann = add_user(&app, "ann", "viewer").await;
    let bob = add_user(&app, "bob", "viewer").await;
    store::create(core, &input("Shared", Visibility::Users, &[ann, bob]))
        .await
        .unwrap();
    assert_eq!(app.count("SELECT COUNT(*) FROM service_users").await, 2);
    app.exec(format!("DELETE FROM users WHERE id = {ann}"))
        .await;
    assert_eq!(app.count("SELECT COUNT(*) FROM service_users").await, 1);
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 1);
}

#[tokio::test]
async fn changing_the_visibility_changes_who_is_listed() {
    let app = App::new().await;
    let core = &app.magpie.core;
    let ann = add_user(&app, "ann", "viewer").await;
    let bob = add_user(&app, "bob", "viewer").await;
    let id = store::create(core, &input("S", Visibility::Users, &[ann]))
        .await
        .unwrap();
    store::update(core, id, &input("S", Visibility::Users, &[bob]))
        .await
        .unwrap();
    assert!(
        !store::can_see(core, Viewer::new(ann, false), id)
            .await
            .unwrap()
    );
    assert!(
        store::can_see(core, Viewer::new(bob, false), id)
            .await
            .unwrap()
    );
    // Leaving "selected users" drops the list.
    store::update(core, id, &input("S", Visibility::Admin, &[bob]))
        .await
        .unwrap();
    assert_eq!(app.count("SELECT COUNT(*) FROM service_users").await, 0);
}

// ---------------------------------------------------------------------------
// The store

#[tokio::test]
async fn the_store_hands_out_the_secret_only_through_a_fetch_context() {
    let app = App::new().await;
    let core = &app.magpie.core;
    let mut with_secret = input("S", Visibility::Admin, &[]);
    with_secret.secret = SecretChange::Replace(SECRET.into());
    let id = store::create(core, &with_secret).await.unwrap();
    let service = store::get(core, id).await.unwrap().unwrap();
    assert!(service.has_secret);
    assert!(!format!("{service:?}").contains(SECRET));
    let ctx = store::fetch_ctx(core, &service).await.unwrap();
    assert_eq!(ctx.secret.as_deref(), Some(SECRET));
    assert_eq!(ctx.settings["url"], "http://10.0.0.5/");
    // Debug prints neither the secret nor the settings.
    let printed = format!("{ctx:?}");
    assert!(!printed.contains(SECRET), "{printed}");
    assert!(!printed.contains("10.0.0.5"), "{printed}");
    // A sealed value that cannot be opened (the key changed) is an error, not
    // a fetch without a secret.
    app.exec(format!("UPDATE services SET secret = 'sealed:v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA' WHERE id = {id}"))
        .await;
    let broken = store::get(core, id).await.unwrap().unwrap();
    let err = store::fetch_ctx(core, &broken).await.unwrap_err();
    assert_eq!(
        err.fetch_error(),
        integrations::FetchError::SecretUnreadable
    );
}

#[tokio::test]
async fn the_handlers_announce_changes_after_they_are_stored() {
    let app = App::new().await;
    let mut events = app.magpie.service_events.subscribe();
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Blog", "http://10.0.0.5/", "").await;
    assert_eq!(events.try_recv().unwrap(), ServiceEvent::Created(id));
    admin
        .post(
            &format!("/settings/services/{id}"),
            &form("Blog 2", "http://10.0.0.5/", ""),
        )
        .await;
    assert_eq!(events.try_recv().unwrap(), ServiceEvent::Updated(id));
    admin
        .post(&format!("/settings/services/{id}/delete"), &[])
        .await;
    assert_eq!(events.try_recv().unwrap(), ServiceEvent::Deleted(id));
    // A refused form announces nothing.
    admin
        .post("/settings/services/new", &form("", "http://10.0.0.5/", ""))
        .await;
    assert!(events.try_recv().is_err());
}

// ---------------------------------------------------------------------------
// The http kind

#[tokio::test]
async fn http_maps_200_to_ok_and_500_to_bad() {
    let base = stub().await;
    let ok = fetch(&format!("{base}/ok"), None).await;
    assert_eq!(ok.status, Status::Ok);
    assert_eq!(ok.error, None);
    assert_eq!(ok.values["status"].num, Some(200.0));
    assert!(ok.values["latency_ms"].num.is_some());
    assert!(ok.values["checked"].num.is_some());
    assert!(ok.needs.is_empty());

    let bad = fetch(&format!("{base}/fail"), None).await;
    assert_eq!(bad.status, Status::Bad);
    assert_eq!(bad.values["status"].num, Some(500.0));
    assert!(bad.error.as_deref().unwrap().contains("500"));
}

#[tokio::test]
async fn http_follows_the_expected_status_setting() {
    let base = stub().await;
    let mut c = ctx(&format!("{base}/teapot"), None);
    // 418 is outside the default 200-399.
    assert_eq!(http_kind().fetch(&c).await.unwrap().status, Status::Bad);
    for expected in ["418", "400-499", "200-399, 418"] {
        c.settings = json!({ "url": format!("{base}/teapot"), "expected_status": expected });
        assert_eq!(
            http_kind().fetch(&c).await.unwrap().status,
            Status::Ok,
            "{expected}"
        );
    }
    c.settings = json!({ "url": format!("{base}/ok"), "expected_status": "500" });
    assert_eq!(http_kind().fetch(&c).await.unwrap().status, Status::Bad);
}

#[tokio::test]
async fn http_times_out_as_bad_with_an_error() {
    let base = stub().await;
    let mut c = ctx(&format!("{base}/slow"), Some(SECRET));
    c.http = integrations::build_client(Duration::from_millis(300));
    let snap = http_kind().fetch(&c).await.unwrap();
    assert_eq!(snap.status, Status::Bad);
    let error = snap.error.expect("an error");
    assert!(error.to_lowercase().contains("time"), "{error}");
    assert!(!error.contains(SECRET));
    assert!(
        !error.contains("127.0.0.1"),
        "the URL is not in the error: {error}"
    );
}

#[tokio::test]
async fn http_follows_at_most_three_redirects() {
    let base = stub().await;
    let three = fetch(&format!("{base}/hop/3"), None).await;
    assert_eq!(three.status, Status::Ok, "{:?}", three.error);
    let four = fetch(&format!("{base}/hop/4"), None).await;
    assert_eq!(four.status, Status::Bad);
    assert!(four.error.unwrap().contains("redirect"));
    let looping = fetch(&format!("{base}/loop"), None).await;
    assert_eq!(looping.status, Status::Bad);
}

#[tokio::test]
async fn http_does_not_follow_a_redirect_to_the_metadata_address() {
    let base = stub().await;
    let snap = fetch(&format!("{base}/meta"), Some(SECRET)).await;
    assert_eq!(snap.status, Status::Bad);
    let error = snap.error.unwrap();
    assert!(error.contains("not allowed"), "{error}");
    assert!(
        !error.contains("169.254") && !error.contains(SECRET),
        "{error}"
    );
}

#[tokio::test]
async fn http_stops_reading_a_huge_body() {
    let base = stub().await;
    let snap = fetch(&format!("{base}/big"), None).await;
    assert_eq!(snap.status, Status::Bad);
    assert!(snap.error.unwrap().contains("too large"));
}

#[tokio::test]
async fn http_sends_the_secret_as_a_bearer_token_and_keeps_it_out_of_errors() {
    let base = stub().await;
    let good = fetch(&format!("{base}/auth/{SECRET}"), Some(SECRET)).await;
    assert_eq!(good.status, Status::Ok);
    let wrong = fetch(&format!("{base}/auth/other"), Some(SECRET)).await;
    assert_eq!(wrong.status, Status::Bad);
    assert_eq!(wrong.values["status"].num, Some(401.0));
    let text = format!("{wrong:?}");
    assert!(!text.contains(SECRET), "{text}");
    // A closed port: a short reason, no address.
    let closed = fetch("http://127.0.0.1:1/", Some(SECRET)).await;
    assert_eq!(closed.status, Status::Bad);
    let error = closed.error.unwrap();
    assert!(
        !error.contains("127.0.0.1") && !error.contains(SECRET),
        "{error}"
    );
}

#[tokio::test]
async fn http_refuses_urls_with_credentials_or_other_schemes() {
    for url in [
        "http://user:pw@10.0.0.5/",
        "ftp://10.0.0.5/",
        "not a url",
        "",
    ] {
        let result = http_kind().fetch(&ctx(url, None)).await;
        let Err(err) = result else {
            panic!("{url:?} should be refused");
        };
        assert!(!err.to_string().contains("pw"), "{err}");
    }
}

#[tokio::test]
async fn the_http_kind_describes_itself() {
    let kind = http_kind();
    assert_eq!(kind.kind(), "http");
    assert_eq!(kind.label_key(), "kind.http");
    assert_eq!(kind.default_interval(), Duration::from_secs(60));
    assert!(kind.min_interval() <= kind.default_interval());
    let ids: Vec<_> = kind.metrics().iter().map(|m| m.id).collect();
    assert_eq!(ids, ["status", "latency_ms", "checked"]);
    assert!(kind.fields().iter().any(|f| f.secret));
    assert!(integrations::registry().iter().any(|k| k.kind() == "http"));
    assert!(integrations::find("nope").is_none());
}

#[tokio::test]
async fn fetch_ctx_debug_redacts_the_secret() {
    let c = FetchCtx {
        http: integrations::client().clone(),
        settings: json!({ "url": "http://u:p@host/", "token": "inside-settings" }),
        secret: Some(SECRET.into()),
    };
    let printed = format!("{c:?}");
    assert!(!printed.contains(SECRET), "{printed}");
    assert!(!printed.contains("u:p@"), "{printed}");
    assert!(!printed.contains("inside-settings"), "{printed}");
    assert!(
        printed.contains("redacted") || printed.contains("***"),
        "{printed}"
    );
    let none = FetchCtx { secret: None, ..c };
    assert!(format!("{none:?}").contains("secret: None"));
}

#[tokio::test]
async fn the_services_migration_is_in_place() {
    let app = App::new().await;
    let columns: Vec<String> = app
        .magpie
        .core
        .with_conn(|c| {
            let mut s = c.prepare("SELECT name FROM pragma_table_info('services')")?;
            let rows = s.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect()
        })
        .await
        .unwrap();
    for want in [
        "id",
        "kind",
        "name",
        "url",
        "settings",
        "secret",
        "interval_s",
        "visibility",
        "created_at",
        "updated_at",
    ] {
        assert!(
            columns.iter().any(|c| c == want),
            "missing {want}: {columns:?}"
        );
    }
    // The visibility CHECK refuses anything else.
    let bad = app
        .magpie
        .core
        .with_conn(|c| {
            c.execute(
                "INSERT INTO services (kind, name, settings, interval_s, visibility, created_at, updated_at) \
                 VALUES ('http', 'x', '{}', 60, 'everyone', '', '')",
                [],
            )
        })
        .await;
    assert!(bad.is_err());
}

/// A resolver that refuses every name the way the kit's guarded one does.
struct Refuse;

impl reqwest::dns::Resolve for Refuse {
    fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async {
            Err(Box::new(vole_kit::net::Forbidden) as Box<dyn std::error::Error + Send + Sync>)
        })
    }
}

#[tokio::test]
async fn a_name_that_resolves_to_a_forbidden_address_is_called_forbidden() {
    let client = reqwest::Client::builder()
        .no_proxy()
        .dns_resolver(std::sync::Arc::new(Refuse))
        .build()
        .unwrap();
    let err = client
        .get("http://metadata.example.invalid/")
        .send()
        .await
        .unwrap_err();
    assert_eq!(
        integrations::classify(&err),
        integrations::FetchError::Forbidden
    );
}

#[tokio::test]
async fn the_service_routes_are_behind_the_csrf_guard() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    // A form post from another site.
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form("Blog", "http://10.0.0.5/", ""))
        .finish();
    for path in ["/settings/services/new", "/settings/services/test"] {
        let req = axum::http::Request::post(path)
            .header(header::ORIGIN, "http://evil.example")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(body.clone()))
            .unwrap();
        let res = admin.send(req).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(app.count("SELECT COUNT(*) FROM services").await, 0);
}

// ---------------------------------------------------------------------------
// A stored secret never follows a changed address (Ruling G)

type AuthLog = std::sync::Arc<std::sync::Mutex<Vec<Option<String>>>>;

async fn record(State(log): State<AuthLog>, headers: HeaderMap) -> &'static str {
    let sent = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    log.lock().unwrap().push(sent);
    "recorded"
}

/// A stub that records the `Authorization` header of every request to `/rec`
/// and `/rec2`; `/jump/{port}` redirects to `/rec` on another stub.
async fn recorder() -> (String, u16, AuthLog) {
    let log = AuthLog::default();
    let app = Router::new()
        .route("/rec", get(record))
        .route("/rec2", get(record))
        .route(
            "/jump/{port}",
            get(|Path(port): Path<u16>| async move {
                Redirect::temporary(&format!("http://127.0.0.1:{port}/rec"))
            }),
        )
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), addr.port(), log)
}

#[tokio::test]
async fn a_new_address_with_an_empty_secret_is_refused_and_the_secret_stays() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Blog", "http://10.0.0.5/health", SECRET).await;
    let before = stored_secret(&app, id).await.unwrap();
    for new_url in [
        "http://10.0.0.6/health",      // another host
        "http://10.0.0.5:8080/health", // another port
        "https://10.0.0.5/health",     // another scheme
    ] {
        let res = admin
            .post(
                &format!("/settings/services/{id}"),
                &form("Blog", new_url, ""),
            )
            .await;
        assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{new_url}");
        assert!(
            res.body
                .contains("Type the key again when the address changes"),
            "{new_url}: {}",
            res.body
        );
        assert_clean(&res, "the refusal");
        assert_eq!(stored_secret(&app, id).await.unwrap(), before, "{new_url}");
        assert_eq!(
            app.count("SELECT COUNT(*) FROM services WHERE url = 'http://10.0.0.5/health'")
                .await,
            1,
            "the address did not change"
        );
    }
}

#[tokio::test]
async fn the_store_refuses_it_too() {
    let app = App::new().await;
    let core = &app.magpie.core;
    let mut with_secret = input("S", Visibility::Admin, &[]);
    with_secret.secret = SecretChange::Replace(SECRET.into());
    let id = store::create(core, &with_secret).await.unwrap();
    let mut moved = input("S", Visibility::Admin, &[]);
    moved.url = Some("http://10.0.0.99/".into());
    match store::update(core, id, &moved).await {
        Err(store::StoreError::Invalid(errors)) => {
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].field, "secret");
            assert_eq!(errors[0].key, "err.secret_origin");
        }
        other => panic!("{other:?}"),
    }
    // A service that has no secret may move freely.
    let none = store::create(core, &input("N", Visibility::Admin, &[]))
        .await
        .unwrap();
    store::update(core, none, &moved).await.unwrap();
}

#[tokio::test]
async fn the_same_origin_with_another_path_keeps_the_secret() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Blog", "http://10.0.0.5/health", SECRET).await;
    let before = stored_secret(&app, id).await.unwrap();
    // Another path, and the default port written out: still the same origin.
    for new_url in ["http://10.0.0.5/other?x=1", "http://10.0.0.5:80/health"] {
        let res = admin
            .post(
                &format!("/settings/services/{id}"),
                &form("Blog", new_url, ""),
            )
            .await;
        assert_eq!(res.status, StatusCode::SEE_OTHER, "{new_url}: {}", res.body);
        assert_eq!(stored_secret(&app, id).await.unwrap(), before);
    }
}

#[tokio::test]
async fn clearing_or_replacing_the_secret_allows_a_new_address() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Blog", "http://10.0.0.5/", SECRET).await;
    // Replace with a new origin.
    let res = admin
        .post(
            &format!("/settings/services/{id}"),
            &form("Blog", "http://10.0.0.6/", "brand-new-secret"),
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let now = stored_secret(&app, id).await.unwrap();
    assert_eq!(
        vole_kit::crypto::open(&app.magpie.core.session_key, PURPOSE, &now).as_deref(),
        Ok("brand-new-secret")
    );
    // Clear with another new origin.
    let mut f = form("Blog", "http://10.0.0.7/", "");
    f.push(("clear_secret", "true"));
    let res = admin.post(&format!("/settings/services/{id}"), &f).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(stored_secret(&app, id).await, None);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM services WHERE url = 'http://10.0.0.7/'")
            .await,
        1
    );
}

#[tokio::test]
async fn test_never_sends_a_stored_secret_to_another_address() {
    let (a, _, log_a) = recorder().await;
    let (b, _, log_b) = recorder().await;
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Guarded", &format!("{a}/rec"), SECRET).await;
    let id_text = id.to_string();
    let (url_b, url_a2) = (format!("{b}/rec"), format!("{a}/rec2"));

    // Another port: refused, and nothing is sent to either stub.
    let mut f = form("Guarded", &url_b, "");
    f.push(("id", &id_text));
    let res = admin.post("/settings/services/test", &f).await;
    assert!(
        res.body.contains("data-test-status=\"unknown\""),
        "{}",
        res.body
    );
    assert!(
        res.body
            .contains("Type the key again when the address changes")
    );
    assert_clean(&res, "the refusal");
    assert!(log_a.lock().unwrap().is_empty());
    assert!(log_b.lock().unwrap().is_empty());

    // A key typed in the form may go to the new address.
    let mut f = form("Guarded", &url_b, "typed-key-123");
    f.push(("id", &id_text));
    let res = admin.post("/settings/services/test", &f).await;
    assert!(res.body.contains("data-test-status=\"ok\""), "{}", res.body);
    assert_eq!(
        *log_b.lock().unwrap(),
        [Some("Bearer typed-key-123".to_string())]
    );

    // Removing the key goes without one.
    let mut f = form("Guarded", &url_b, "");
    f.push(("id", &id_text));
    f.push(("clear_secret", "true"));
    admin.post("/settings/services/test", &f).await;
    assert_eq!(log_b.lock().unwrap().last(), Some(&None));

    // The same origin with another path uses the stored key.
    let mut f = form("Guarded", &url_a2, "");
    f.push(("id", &id_text));
    let res = admin.post("/settings/services/test", &f).await;
    assert!(res.body.contains("data-test-status=\"ok\""), "{}", res.body);
    assert_eq!(*log_a.lock().unwrap(), [Some(format!("Bearer {SECRET}"))]);
    assert_clean(&res, "the test result");
}

#[tokio::test]
async fn a_redirect_to_another_origin_drops_the_authorization_header() {
    let (a, _, log_a) = recorder().await;
    let (_, port_b, log_b) = recorder().await;
    let snap = fetch(&format!("{a}/jump/{port_b}"), Some(SECRET)).await;
    assert_eq!(snap.status, Status::Ok, "{:?}", snap.error);
    assert!(log_a.lock().unwrap().is_empty(), "A only redirected");
    assert_eq!(
        *log_b.lock().unwrap(),
        [None],
        "B got no Authorization header"
    );
}

#[tokio::test]
async fn disguised_metadata_addresses_are_refused_directly_and_as_redirects() {
    let base = stub().await;
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    for url in [
        "http://2852039166/",
        "http://0251.0376.0251.0376/",
        "http://[::ffff:169.254.169.254]/",
        "http://[fe80::1]/",
    ] {
        // «Test»
        let res = admin
            .post("/settings/services/test", &form("Cloud", url, SECRET))
            .await;
        assert!(
            res.body.contains("data-test-status=\"bad\""),
            "{url}: {}",
            res.body
        );
        assert!(res.body.contains("not allowed"), "{url}");
        assert_no_header(&res, url);
        // As the target of a redirect.
        let hop = format!(
            "{base}/to?u={}",
            url.replace('[', "%5B").replace(']', "%5D")
        );
        let snap = fetch(&hop, Some(SECRET)).await;
        assert_eq!(snap.status, Status::Bad, "{url}");
        let error = snap.error.unwrap();
        assert!(error.contains("not allowed"), "{url}: {error}");
        assert!(!error.contains(SECRET));
    }
}

#[tokio::test]
async fn an_unreadable_stored_secret_is_said_not_shown_as_saved() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Blog", "http://10.0.0.5/", SECRET).await;
    app.exec(format!(
        "UPDATE services SET secret = 'sealed:v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA' WHERE id = {id}"
    ))
    .await;
    let edit = admin.get(&format!("/settings/services/{id}")).await;
    assert_eq!(edit.status, StatusCode::OK);
    assert!(
        edit.body.contains("can no longer be opened"),
        "{}",
        edit.body
    );
    assert!(!edit.body.contains("Saved – type to replace"));
    // «Test» with the form's empty field says so too, and sends nothing.
    let mut f = form("Blog", "http://10.0.0.5/", "");
    let id_text = id.to_string();
    f.push(("id", &id_text));
    let res = admin.post("/settings/services/test", &f).await;
    assert!(
        res.body.contains("data-test-status=\"unknown\""),
        "{}",
        res.body
    );
    assert!(res.body.contains("can no longer be read"), "{}", res.body);
}

#[tokio::test]
async fn a_refused_secret_is_said_before_the_unreadable_note() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Blog", "http://10.0.0.5/", SECRET).await;
    app.exec(format!(
        "UPDATE services SET secret = 'sealed:v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA' WHERE id = {id}"
    ))
    .await;
    // A new value with a space in it: refused, and the reason shows.
    let res = admin
        .post(
            &format!("/settings/services/{id}"),
            &form("Blog", "http://10.0.0.5/", "not valid"),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
    assert!(
        res.body.contains("visible ASCII characters"),
        "{}",
        res.body
    );
    assert!(
        !res.body.contains("can no longer be opened"),
        "{}",
        res.body
    );
    // The same with a changed address: the origin rule says so.
    let res = admin
        .post(
            &format!("/settings/services/{id}"),
            &form("Blog", "http://10.0.0.99/", ""),
        )
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
    assert!(
        res.body
            .contains("Type the key again when the address changes."),
        "{}",
        res.body
    );
    assert!(
        !res.body.contains("can no longer be opened"),
        "{}",
        res.body
    );
    // With no error the note stays.
    let edit = admin.get(&format!("/settings/services/{id}")).await;
    assert!(edit.body.contains("can no longer be opened"));
}

#[tokio::test]
async fn the_interval_input_has_the_kinds_floor_as_its_minimum() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let res = admin.get("/settings/services/new?kind=http").await;
    assert!(res.body.contains("name=\"interval\""));
    assert!(res.body.contains("min=\"15\""), "{}", res.body);
}

#[tokio::test]
async fn update_and_delete_are_behind_the_csrf_guard_too() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    let id = create_via_page(&mut admin, "Blog", "http://10.0.0.5/", SECRET).await;
    let before = stored_secret(&app, id).await;
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form("Taken over", "http://10.0.0.5/", "evil-secret"))
        .finish();
    for path in [
        format!("/settings/services/{id}"),
        format!("/settings/services/{id}/delete"),
    ] {
        let req = axum::http::Request::post(&path)
            .header(header::ORIGIN, "http://evil.example")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(body.clone()))
            .unwrap();
        let res = admin.send(req).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(
        app.count("SELECT COUNT(*) FROM services WHERE name = 'Blog'")
            .await,
        1
    );
    assert_eq!(stored_secret(&app, id).await, before);
}

// ---------------------------------------------------------------------------
// Logs

#[derive(Clone, Default)]
struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

#[tokio::test]
async fn nothing_that_is_logged_holds_a_secret() {
    // Everything on every thread, at debug level (what RUST_LOG=debug shows).
    // This is the only test that installs a subscriber.
    let capture = Capture::default();
    // `try_init` also bridges the `log` records of reqwest.
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(capture.clone())
        .try_init()
        .expect("no other subscriber in this test binary");
    const FIRST: &str = "probe-secret-first-4f7a91";
    const SECOND: &str = "probe-secret-second-b3c8d2";
    let base = stub().await;
    let app = App::new().await;
    let mut admin = Web::new(&app, "root", "admin").await;
    // Create, update (same origin), a refused update, «Test» three ways, and a
    // failed fetch.
    let url = format!("{base}/auth/{SECOND}");
    let id = create_via_page(&mut admin, "Probe", &url, FIRST).await;
    let path = format!("/settings/services/{id}");
    admin.post(&path, &form("Probe", &url, SECOND)).await;
    admin
        .post(&path, &form("Probe", "http://10.9.9.9/", ""))
        .await;
    for target in [
        url.clone(),
        format!("{base}/fail"),
        "http://127.0.0.1:1/".into(),
    ] {
        admin
            .post("/settings/services/test", &form("Probe", &target, SECOND))
            .await;
    }
    let ctx = FetchCtx {
        http: integrations::client().clone(),
        settings: json!({ "url": "http://127.0.0.1:1/" }),
        secret: Some(SECOND.into()),
    };
    let snap = http_kind().fetch(&ctx).await.unwrap();
    assert_eq!(snap.status, Status::Bad);
    tracing::debug!(ctx = ?ctx, "a debug print of the fetch context");
    admin.post(&format!("{path}/delete"), &[]).await;

    let log = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    assert!(log.contains("service created"), "the capture works:\n{log}");
    assert!(log.contains("service tested"), "{log}");
    // The libraries' own lines are in the capture, so it would catch a leak
    // from them too.
    assert!(
        log.contains("reqwest") || log.contains("hyper_util"),
        "no library line was captured:
{log}"
    );
    for needle in [FIRST, SECOND, "Bearer"] {
        assert!(!log.contains(needle), "the log holds {needle:?}:\n{log}");
    }
}
