//! `GET /api/v1/me/start`: the token owner's layout for the browser
//! extension. Only an API token opens it; a session cookie never does.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{App, HOST, Web, bearer_get, make_token, user_id};
use serde_json::{Value, json};
use vole_kit::auth::TokenScope;

const PATH: &str = "/api/v1/me/start";

fn doc(version: u64, title: &str) -> Value {
    json!({
        "version": version,
        "background": { "kind": "preset", "name": "hav" },
        "groups": [{
            "id": "g1",
            "title": "Services",
            "cards": [{
                "id": "c1",
                "title": title,
                "description": "Uptime",
                "url": "https://vole.example.com/",
                "icon": { "emoji": null, "colour": "teal" }
            }]
        }],
        "bookmarks": [{ "id": "b1", "title": "Docs", "url": "https://docs.example.com/" }]
    })
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

#[tokio::test]
async fn a_read_token_gets_its_owners_layout() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    save(&mut web, &doc(0, "Vigilant Vole")).await;
    let token = make_token(&app, user_id(&app, "ada").await, TokenScope::Read, "t").await;

    let res = bearer_get(&app, PATH, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.header("cache-control"), "no-store");
    assert!(res.header("content-type").starts_with("application/json"));
    let layout = res.json();
    assert_eq!(layout["version"], 1);
    assert_eq!(layout["groups"][0]["cards"][0]["title"], "Vigilant Vole");
    assert_eq!(layout["bookmarks"][0]["title"], "Docs");
}

#[tokio::test]
async fn a_write_token_reads_too() {
    let app = App::new().await;
    Web::new(&app, "ada", "admin").await;
    let token = make_token(&app, user_id(&app, "ada").await, TokenScope::Write, "t").await;
    let res = bearer_get(&app, PATH, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

#[tokio::test]
async fn another_users_token_gets_only_its_own_layout() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    save(&mut ada, &doc(0, "Ada's card")).await;
    let mut bob = Web::new(&app, "bob", "viewer").await;
    let bob_token = make_token(&app, user_id(&app, "bob").await, TokenScope::Read, "t").await;

    // Bob has saved nothing: the empty default, not Ada's page.
    let res = bearer_get(&app, PATH, &bob_token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let layout = res.json();
    assert_eq!(layout["version"], 0);
    assert_eq!(layout["groups"], json!([]));
    assert!(!res.body.contains("Ada's card"));

    save(&mut bob, &doc(0, "Bob's card")).await;
    let res = bearer_get(&app, PATH, &bob_token).await;
    assert_eq!(res.json()["groups"][0]["cards"][0]["title"], "Bob's card");
}

#[tokio::test]
async fn no_invalid_or_revoked_token_is_401() {
    let app = App::new().await;
    Web::new(&app, "ada", "admin").await;

    let res = app.get(PATH).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);

    let junk = format!("mm_{}", "x".repeat(43));
    for bad in [junk.as_str(), "mm_short", "not-a-token", ""] {
        let res = bearer_get(&app, PATH, bad).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{bad}: {}", res.body);
    }

    let token = make_token(&app, user_id(&app, "ada").await, TokenScope::Read, "t").await;
    assert_eq!(bearer_get(&app, PATH, &token).await.status, StatusCode::OK);
    app.exec("DELETE FROM api_tokens".into()).await;
    let res = bearer_get(&app, PATH, &token).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
}

#[tokio::test]
async fn a_session_cookie_alone_is_not_enough() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let res = web.get(PATH).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
    // Not even with the headers of the app's own pages.
    let req = Request::get(PATH)
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let res = web.send(req).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
}

#[tokio::test]
async fn an_unreadable_stored_layout_is_the_default_at_the_stored_version() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    save(&mut web, &doc(0, "Fine")).await;
    let uid = user_id(&app, "ada").await;
    app.exec(format!(
        "UPDATE layouts SET doc = 'not json', version = 7 WHERE user_id = {uid}"
    ))
    .await;
    let token = make_token(&app, uid, TokenScope::Read, "t").await;

    let res = bearer_get(&app, PATH, &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.header("cache-control"), "no-store");
    let layout = res.json();
    assert_eq!(layout["version"], 7);
    assert_eq!(layout["groups"], json!([]));
}
