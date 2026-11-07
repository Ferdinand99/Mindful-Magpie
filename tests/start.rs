//! The start page: it renders for a signed-in user in the Hedgerow layout,
//! never takes focus (the address bar keeps it), and its grid fills the width.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{App, Web};

/// `GET /` as a browser with `Accept-Language: lang`.
async fn start_page(web: &mut Web<'_>, lang: &str) -> common::Res {
    let req = Request::get("/")
        .header("accept-language", lang)
        .body(Body::empty())
        .unwrap();
    web.send(req).await
}

#[tokio::test]
async fn start_page_renders_for_signed_in_user() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;

    let res = start_page(&mut web, "nb").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains(r#"id="groups""#), "{}", res.body);
    assert!(res.body.contains(r#"id="needs""#), "{}", res.body);
    assert!(res.body.contains(r#"id="clock""#), "{}", res.body);
    assert!(
        res.body
            .contains("Startsiden er tom. Rediger den for å legge til tjenester og bokmerker."),
        "{}",
        res.body
    );

    let res = start_page(&mut web, "en").await;
    assert!(
        res.body
            .contains("Your start page is empty. Edit it to add services and bookmarks."),
        "{}",
        res.body
    );
}

#[tokio::test]
async fn start_page_needs_sign_in() {
    let app = App::new().await;
    // With a user but no session, `/` sends the visitor to sign in.
    common::add_user(&app, "ada", "admin").await;
    let res = app.get("/").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.header("location"), "/login");
}

#[tokio::test]
async fn boosted_navigation_loads_the_page_in_full() {
    // htmx swaps in the body of a boosted request but loads neither the
    // page's stylesheet nor its script, so the browser reloads `/` in full.
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let req = Request::get("/")
        .header("hx-request", "true")
        .header("hx-boosted", "true")
        .body(Body::empty())
        .unwrap();
    let res = web.send(req).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.header("hx-redirect"), "/");
}

#[tokio::test]
async fn start_page_never_takes_focus() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let res = start_page(&mut web, "en").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let html = res.body.to_lowercase();

    assert!(!html.contains("autofocus"), "autofocus on the page");
    assert!(!html.contains(".focus("), "a script moves focus");

    // No input elements, except the kit's theme picker in the account menu
    // (radio buttons named `vv-theme`; they sit in a closed popover).
    for (at, _) in html.match_indices("<input") {
        let tag = &html[at..html[at..].find('>').map_or(html.len(), |end| at + end)];
        assert!(
            tag.contains(r#"name="vv-theme""#),
            "an input element on the page: {tag}"
        );
    }

    // Page scripts are static files (the CSP allows no inline scripts), and
    // the clock script never moves focus either.
    assert!(
        html.contains(r#"<script src="/static/clock.js""#),
        "the clock script is not loaded"
    );
    let js = app.get("/static/clock.js").await;
    assert_eq!(js.status, StatusCode::OK, "{}", js.body);
    assert!(
        js.header("content-type").contains("javascript"),
        "content-type: {}",
        js.header("content-type")
    );
    assert!(!js.body.contains(".focus("), "clock.js moves focus");
    assert!(!js.body.contains("autofocus"), "clock.js sets autofocus");
}

#[tokio::test]
async fn grid_fills_the_width() {
    let app = App::new().await;
    let res = app.get("/static/magpie.css").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let css = res.body;
    assert!(
        css.contains("repeat(auto-fill, minmax(260px, 1fr))"),
        "the group grid is not auto-fill"
    );

    // No rule for `#groups` caps the width (at-rules nest, so read the text
    // between braces: a selector runs from the last brace to its `{`).
    let mut selector_start = 0;
    for (at, c) in css.char_indices() {
        match c {
            '{' => {
                let selector = &css[selector_start..at];
                let body = &css[at..css[at..].find('}').map_or(css.len(), |end| at + end)];
                if selector.contains("#groups") {
                    assert!(
                        !body.contains("max-width") && !body.contains("max-inline-size"),
                        "`#groups` has a fixed width cap: {selector}{body}"
                    );
                }
                selector_start = at + 1;
            }
            '}' => selector_start = at + 1,
            _ => {}
        }
    }
}
