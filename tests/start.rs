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
        res.body.contains(r#"href="/static/magpie/render.css""#),
        "{}",
        res.body
    );
    // The renderer draws the empty state (the text is in the labels block).
    assert!(!res.body.contains("empty-card"), "{}", res.body);
    // The root the renderer fills carries the hook the background styles need.
    assert!(
        res.body.contains(r#"<section id="groups" class="mm-root""#),
        "{}",
        res.body
    );
    // Without scripts the page says why it is bare.
    assert!(
        res.body.contains("<noscript>") && res.body.contains("Startsiden trenger JavaScript"),
        "{}",
        res.body
    );
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
    assert!(
        res.body.contains("The start page needs JavaScript"),
        "{}",
        res.body
    );
}

#[tokio::test]
async fn background_rules_only_apply_where_the_root_is() {
    // htmx swaps only the body, so `<html>` outlives a boosted visit to another
    // page. Every rule that paints <html> or forces its colour scheme must
    // need the renderer's root (`.mm-root`) to be in the page.
    let app = App::new().await;
    let res = app.get("/static/magpie/render.css").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    // Comments out first: they talk about these selectors too.
    let mut css = res.body;
    while let Some(start) = css.find("/*") {
        let end = css[start..].find("*/").expect("comment never closes") + start + 2;
        css.replace_range(start..end, "");
    }
    let mut selector_start = 0;
    let mut checked = 0;
    for (at, c) in css.char_indices() {
        match c {
            '{' => {
                let selector = css[selector_start..at].trim();
                let body = &css[at..css[at..].find('}').map_or(css.len(), |end| at + end)];
                if selector.contains("data-bg") || selector.starts_with("html") {
                    assert!(
                        selector.contains(":has(.mm-root)"),
                        "this rule outlives the start page: {selector}"
                    );
                    checked += 1;
                }
                if selector.contains("data-bg") {
                    assert!(
                        body.contains("background") || body.contains("color-scheme"),
                        "{selector}"
                    );
                }
                selector_start = at + 1;
            }
            '}' => selector_start = at + 1,
            _ => {}
        }
    }
    assert!(checked >= 6, "only {checked} html rules found");
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
    // The answer depends on the boost header, so caches must keep it apart.
    assert!(
        res.header("vary").contains("HX-Boosted"),
        "vary: {}",
        res.header("vary")
    );
}

#[tokio::test]
async fn boosted_navigation_from_the_start_page_is_not_reloaded() {
    // The start page's head is already loaded when the request comes from `/`:
    // the page is swapped in, with no redirect (that would reload it for nothing).
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    for current in ["http://magpie.test:3010/", "http://magpie.test:3010/?x=1"] {
        let req = Request::get("/")
            .header("hx-request", "true")
            .header("hx-boosted", "true")
            .header("hx-current-url", current)
            .body(Body::empty())
            .unwrap();
        let res = web.send(req).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        assert_eq!(res.header("hx-redirect"), "", "redirected from {current}");
        assert!(res.body.contains(r#"id="groups""#), "{}", res.body);
        assert!(
            res.header("vary").contains("HX-Boosted"),
            "vary: {}",
            res.header("vary")
        );
    }

    // From any other page the redirect stays.
    let req = Request::get("/")
        .header("hx-request", "true")
        .header("hx-boosted", "true")
        .header(
            "hx-current-url",
            "http://magpie.test:3010/settings/security",
        )
        .body(Body::empty())
        .unwrap();
    let res = web.send(req).await;
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
    // none of them moves focus either.
    assert!(
        html.contains(r#"<script src="/static/clock.js""#),
        "the clock script is not loaded"
    );
    assert!(
        html.contains(r#"<script type="module" src="/static/magpie/start.js""#),
        "the start page script is not loaded as a module"
    );
    for path in [
        "/static/clock.js",
        "/static/magpie/start.js",
        "/static/magpie/render.js",
    ] {
        let js = app.get(path).await;
        assert_eq!(js.status, StatusCode::OK, "{path}: {}", js.body);
        assert!(
            js.header("content-type").contains("javascript"),
            "{path} content-type: {}",
            js.header("content-type")
        );
    }

    // Every script of the page's own, read from disk so that a new file is
    // covered at once. Only edit mode (`edit.js`, which the user opens on
    // purpose) may move focus, and nothing sets `autofocus`.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static");
    let mut scripts = vec![dir.join("clock.js")];
    for entry in std::fs::read_dir(dir.join("magpie")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "js") && path.file_name().unwrap() != "edit.js" {
            scripts.push(path);
        }
    }
    assert!(
        scripts.iter().any(|p| p.ends_with("render.js")),
        "render.js is not among the scripts checked: {scripts:?}"
    );
    for path in scripts {
        let source = std::fs::read_to_string(&path).unwrap().to_lowercase();
        assert!(
            !source.contains(".focus("),
            "{} moves focus",
            path.display()
        );
        assert!(
            !source.contains("autofocus"),
            "{} sets autofocus",
            path.display()
        );
    }
}

#[tokio::test]
async fn grid_fills_the_width() {
    let app = App::new().await;
    let mut css = String::new();
    for path in ["/static/magpie.css", "/static/magpie/render.css"] {
        let res = app.get(path).await;
        assert_eq!(res.status, StatusCode::OK, "{path}: {}", res.body);
        css.push_str(&res.body);
        css.push('\n');
    }
    // The renderer's card grid fills the width: as many 260px columns as fit.
    assert!(
        css.contains("repeat(auto-fill, minmax(260px, 1fr))"),
        "the card grid is not auto-fill"
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

/// The JSON inside `<script type="application/json" id="{id}">`, found the
/// way a browser finds it: the block ends at the first `</script>`.
fn json_block(page: &str, id: &str) -> serde_json::Value {
    let open = format!(r#"<script type="application/json" id="{id}">"#);
    let start = page
        .find(&open)
        .unwrap_or_else(|| panic!("no {id} block in {page}"))
        + open.len();
    // A browser ends the block at `</script` in any case, whatever follows.
    let end = page[start..]
        .to_ascii_lowercase()
        .find("</script")
        .expect("block never closes")
        + start;
    serde_json::from_str(&page[start..end]).unwrap_or_else(|e| panic!("{e}: {}", &page[start..end]))
}

/// Saves a layout with one card titled `title` for the signed-in `web`.
async fn save_card(web: &mut Web<'_>, title: &str) {
    let layout = serde_json::json!({
        "version": 0,
        "background": { "kind": "preset", "name": "hedgerow" },
        "groups": [{
            "id": "g1",
            "title": "Services",
            "cards": [{
                "id": "c1",
                "title": title,
                "description": "",
                "url": "https://vole.example.com/",
                "icon": { "emoji": null, "colour": "teal" }
            }]
        }],
        "bookmarks": []
    });
    let req = Request::post("/start/layout")
        .header("hx-request", "true")
        .header("content-type", "application/json")
        .body(Body::from(layout.to_string()))
        .unwrap();
    let res = web.send(req).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

#[tokio::test]
async fn start_page_embeds_the_users_layout() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;

    // Before any save: the default layout, as data.
    let res = start_page(&mut web, "en").await;
    let layout = json_block(&res.body, "mm-layout");
    assert_eq!(layout["version"], 0);
    assert_eq!(layout["groups"], serde_json::json!([]));
    assert_eq!(layout["background"]["name"], "hedgerow");

    save_card(&mut web, "Vigilant Vole").await;
    let res = start_page(&mut web, "en").await;
    let layout = json_block(&res.body, "mm-layout");
    assert_eq!(layout["version"], 1);
    assert_eq!(layout["groups"][0]["cards"][0]["title"], "Vigilant Vole");
}

#[tokio::test]
async fn hostile_titles_cannot_close_the_json_block() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let hostile = "</SCRIPT x></script ><script>alert(1)</script><!-- <script>";
    save_card(&mut web, hostile).await;

    let res = start_page(&mut web, "en").await;
    // The block parses, and holds the title as text.
    let layout = json_block(&res.body, "mm-layout");
    assert_eq!(layout["groups"][0]["cards"][0]["title"], hostile);
    // The raw text never appears in the page: `</` and `<!--` are escaped.
    assert!(
        !res.body.contains("</script><script>alert(1)"),
        "{}",
        res.body
    );
    assert!(res.body.contains(r"<\/script>"), "{}", res.body);
    assert!(!res.body.contains("<!-- <script>"), "{}", res.body);
    // And the page has no script of its own beyond the static files.
    // (The data blocks are cut out first: their text is data, and `<script`
    // in a title is harmless once `<!--` and `</` are escaped.)
    let mut outside = res.body.clone();
    for id in ["mm-layout", "mm-labels"] {
        let open = format!(r#"<script type="application/json" id="{id}">"#);
        let start = outside.find(&open).unwrap() + open.len();
        let end = outside[start..]
            .to_ascii_lowercase()
            .find("</script")
            .unwrap()
            + start;
        outside.replace_range(start..end, "");
    }
    for (at, _) in outside.match_indices("<script") {
        let tag = &outside[at..outside[at..].find('>').map_or(outside.len(), |e| at + e)];
        assert!(
            tag.contains("src=") || tag.contains(r#"type="application/json""#),
            "an inline script: {tag}"
        );
    }
}

#[tokio::test]
async fn start_page_embeds_the_labels_for_the_renderer() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    for (lang, empty, invalid) in [
        (
            "en",
            "Your start page is empty. Edit it to add services and bookmarks.",
            "invalid link",
        ),
        (
            "nb",
            "Startsiden er tom. Rediger den for å legge til tjenester og bokmerker.",
            "ugyldig lenke",
        ),
    ] {
        let res = start_page(&mut web, lang).await;
        let labels = json_block(&res.body, "mm-labels");
        assert_eq!(labels["empty"], empty, "{lang}");
        assert_eq!(labels["invalid_link"], invalid, "{lang}");
        for key in ["group_label", "bookmarks_label"] {
            let text = labels[key].as_str().unwrap_or_default();
            assert!(!text.is_empty() && !text.contains("start."), "{lang} {key}");
        }
    }
}
