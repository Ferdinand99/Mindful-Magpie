//! Saving the start page layout over HTTP: `POST /start/layout` (sign-in, the
//! kit's CSRF rule, versions, validation, the body limit) and the reset of a
//! layout that cannot be read.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{App, HOST, Res, Web};
use serde_json::{Value, json};

/// A valid document at `version` with one card titled `title`.
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

/// `POST /start/layout` the way the page's own script does it.
async fn save_raw(web: &mut Web<'_>, body: Vec<u8>) -> Res {
    let req = Request::post("/start/layout")
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header("hx-request", "true")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    web.send(req).await
}

async fn save(web: &mut Web<'_>, layout: &Value) -> Res {
    save_raw(web, serde_json::to_vec(layout).unwrap()).await
}

/// The JSON inside `<script type="application/json" id="{id}">`, found the
/// way a browser finds it: the block ends at the first `</script>`.
fn block(page: &str, id: &str) -> Value {
    let open = format!(r#"<script type="application/json" id="{id}">"#);
    let start = page
        .find(&open)
        .unwrap_or_else(|| panic!("no {id} block in {page}"))
        + open.len();
    let end = page[start..].find("</script>").expect("block never closes") + start;
    serde_json::from_str(&page[start..end]).unwrap_or_else(|e| panic!("{e}: {}", &page[start..end]))
}

async fn page(web: &mut Web<'_>) -> Res {
    let res = web.get("/").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    res
}

#[tokio::test]
async fn save_layout_round_trip() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;

    let res = save(&mut web, &doc(0, "Vigilant Vole")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.json(), json!({ "version": 1 }));

    let layout = block(&page(&mut web).await.body, "mm-layout");
    assert_eq!(layout["version"], 1);
    assert_eq!(layout["groups"][0]["cards"][0]["title"], "Vigilant Vole");

    // The next save carries the new version.
    let res = save(&mut web, &doc(1, "Vole")).await;
    assert_eq!(res.json(), json!({ "version": 2 }));
}

#[tokio::test]
async fn save_layout_conflict() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    assert_eq!(
        save(&mut web, &doc(0, "First")).await.status,
        StatusCode::OK
    );

    // A second tab that still holds version 0.
    let res = save(&mut web, &doc(0, "Second")).await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    assert_eq!(res.json(), json!({ "error": "conflict", "current": 1 }));

    // The first save stands.
    let layout = block(&page(&mut web).await.body, "mm-layout");
    assert_eq!(layout["groups"][0]["cards"][0]["title"], "First");
}

#[tokio::test]
async fn save_layout_invalid() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let mut bad = doc(0, "Vole");
    bad["groups"][0]["cards"][0]["url"] = json!("javascript:alert(1)");

    let res = save(&mut web, &bad).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
    assert_eq!(
        res.json(),
        json!({
            "error": "invalid",
            "fields": [{ "path": "groups[0].cards[0].url", "code": "bad_scheme" }]
        })
    );
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 0);
}

#[tokio::test]
async fn an_unreadable_body_is_a_bad_request() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    for body in [
        b"not json".to_vec(),
        b"{}".to_vec(),
        serde_json::to_vec(&json!({ "version": 0, "extra": 1, "background":
            { "kind": "preset", "name": "hav" }, "groups": [], "bookmarks": [] }))
        .unwrap(),
    ] {
        let res = save_raw(&mut web, body).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
        assert_eq!(res.json(), json!({ "error": "bad_request" }));
    }
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 0);
}

#[tokio::test]
async fn save_layout_needs_csrf() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    // Signed in, but nothing shows the request comes from our own page.
    let req = Request::post("/start/layout")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&doc(0, "Vole")).unwrap()))
        .unwrap();
    assert_eq!(web.send(req).await.status, StatusCode::FORBIDDEN);

    // Another site's Origin is no proof either.
    let req = Request::post("/start/layout")
        .header(header::ORIGIN, "https://evil.example")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&doc(0, "Vole")).unwrap()))
        .unwrap();
    assert_eq!(web.send(req).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 0);

    // A same-origin Origin alone is enough (no HX-Request).
    let req = Request::post("/start/layout")
        .header(header::ORIGIN, format!("http://{HOST}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&doc(0, "Vole")).unwrap()))
        .unwrap();
    assert_eq!(web.send(req).await.status, StatusCode::OK);
}

#[tokio::test]
async fn save_layout_needs_sign_in() {
    let app = App::new().await;
    common::add_user(&app, "ada", "admin").await;
    let mut anon = Web::anonymous(&app);
    let res = save(&mut anon, &doc(0, "Vole")).await;
    assert!(
        res.status == StatusCode::SEE_OTHER || res.status == StatusCode::UNAUTHORIZED,
        "{}: {}",
        res.status,
        res.body
    );
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 0);
}

#[tokio::test]
async fn users_cannot_touch_each_other() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let mut bo = Web::new(&app, "bo", "editor").await;

    assert_eq!(save(&mut bo, &doc(0, "Bo's secret")).await.status, 200);
    // Ada is at version 0 still: Bo's save did not move her.
    let ada_page = page(&mut ada).await;
    assert!(!ada_page.body.contains("Bo's secret"), "{}", ada_page.body);
    assert_eq!(block(&ada_page.body, "mm-layout")["version"], 0);

    assert_eq!(save(&mut ada, &doc(0, "Ada's own")).await.status, 200);
    let bo_page = page(&mut bo).await;
    assert!(!bo_page.body.contains("Ada's own"), "{}", bo_page.body);
    assert!(bo_page.body.contains("Bo's secret"));

    // No route takes a user id: the layout routes are fixed paths.
    for path in ["/start/layout/1", "/start/layout/2", "/start/1/layout"] {
        let res = ada.get(path).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn oversized_body_is_refused() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let res = save_raw(&mut web, vec![b' '; 5 * 1024 * 1024]).await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 0);
}

#[tokio::test]
async fn the_largest_honest_document_is_accepted() {
    // Every field at its limit, with four-byte characters in the text: what
    // a real client sends (JSON.stringify does not escape them).
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let url = format!("https://example.com/{}", "a".repeat(2048 - 20));
    let card = |id: String| {
        json!({
            "id": id,
            "title": "😀".repeat(80),
            "description": "😀".repeat(160),
            "url": url,
            "icon": { "emoji": "😀", "colour": "teal" }
        })
    };
    let layout = json!({
        "version": 0,
        "background": { "kind": "preset", "name": "hav" },
        "groups": (0..30).map(|g| json!({
            "id": format!("g{g}"),
            "title": "😀".repeat(60),
            "cards": (0..10).map(|c| card(format!("c{g}-{c}"))).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),
        "bookmarks": (0..100).map(|b| json!({
            "id": format!("b{b}"), "title": "😀".repeat(80), "url": url
        })).collect::<Vec<_>>()
    });
    let body = serde_json::to_vec(&layout).unwrap();
    assert!(body.len() > 1_000_000, "{}", body.len());
    let res = save_raw(&mut web, body).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

/// Stores a document the reader cannot parse, at `version`.
async fn corrupt(app: &App, user: &str, version: i64) {
    app.exec(format!(
        "INSERT INTO layouts (user_id, version, doc, updated_at) \
         SELECT id, {version}, '{{\"groups\": \"not a list\"', '2026-01-01T00:00:00Z' \
         FROM users WHERE username = '{user}'"
    ))
    .await;
}

#[tokio::test]
async fn an_unreadable_layout_shows_a_notice_and_can_be_saved_over() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    corrupt(&app, "ada", 3).await;

    let req = Request::get("/")
        .header("accept-language", "nb")
        .body(Body::empty())
        .unwrap();
    let res = web.send(req).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.body.contains("Startsiden din kunne ikke leses"),
        "{}",
        res.body
    );
    assert!(res.body.contains("Start på nytt"), "{}", res.body);
    // The page still gets a layout: the default, at the stored version, so a
    // save over it is accepted.
    let layout = block(&res.body, "mm-layout");
    assert_eq!(layout["version"], 3);
    assert_eq!(layout["groups"], json!([]));

    let res = save(&mut web, &doc(3, "Fresh")).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.json(), json!({ "version": 4 }));
    let res = page(&mut web).await;
    assert!(!res.body.contains("mm-corrupt"), "{}", res.body);
    assert_eq!(
        block(&res.body, "mm-layout")["groups"][0]["cards"][0]["title"],
        "Fresh"
    );
}

#[tokio::test]
async fn start_over_replaces_an_unreadable_layout() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    corrupt(&app, "ada", 3).await;
    assert!(page(&mut web).await.body.contains("mm-corrupt"));

    let res = web.post("/start/layout/reset", &[]).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.header("location"), "/");
    let res = page(&mut web).await;
    assert!(!res.body.contains("mm-corrupt"), "{}", res.body);
    assert_eq!(block(&res.body, "mm-layout")["version"], 4);
}

#[tokio::test]
async fn start_over_leaves_a_readable_layout_alone() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    assert_eq!(save(&mut web, &doc(0, "Keep me")).await.status, 200);

    let res = web.post("/start/layout/reset", &[]).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let layout = block(&page(&mut web).await.body, "mm-layout");
    assert_eq!(layout["version"], 1);
    assert_eq!(layout["groups"][0]["cards"][0]["title"], "Keep me");

    // And it needs a user and a same-origin proof like every other write.
    let mut anon = Web::anonymous(&app);
    let res = anon.post("/start/layout/reset", &[]).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.header("location"), "/login");
    let req = Request::post("/start/layout/reset")
        .body(Body::empty())
        .unwrap();
    assert_eq!(web.send(req).await.status, StatusCode::FORBIDDEN);
}
