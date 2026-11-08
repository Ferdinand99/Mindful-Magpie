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
    // The row «Trenger deg» is drawn by the renderer into #groups, not
    // placed in the page.
    assert!(!res.body.contains(r#"id="needs""#), "{}", res.body);
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
                    // A selector list: every member needs the root.
                    for member in selector.split(',') {
                        assert!(
                            member.contains(":has(.mm-root)"),
                            "this rule outlives the start page: {member} (in {selector})"
                        );
                        checked += 1;
                    }
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
async fn boosted_navigation_to_the_start_page_always_loads_it_in_full() {
    // htmx drops the page's data blocks (they are scripts) from what it swaps
    // in, so even from the start page itself a boosted request is answered with
    // a redirect: the page is loaded in full, with its data.
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    for current in [
        "http://magpie.test:3010/",
        "http://magpie.test:3010/?x=1",
        "http://magpie.test:3010/settings/security",
    ] {
        let req = Request::get("/")
            .header("hx-request", "true")
            .header("hx-boosted", "true")
            .header("hx-current-url", current)
            .body(Body::empty())
            .unwrap();
        let res = web.send(req).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        assert_eq!(res.header("hx-redirect"), "/", "from {current}");
        assert!(!res.body.contains(r#"id="groups""#), "{}", res.body);
        assert_eq!(res.header("vary"), "HX-Boosted");
    }
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
        for key in [
            "group_label",
            "bookmarks_label",
            "service_missing",
            "service_waiting",
            "status_ok",
            "status_warn",
            "status_bad",
            "status_unknown",
            "updated",
        ] {
            let text = labels[key].as_str().unwrap_or_default();
            assert!(!text.is_empty() && !text.contains("start."), "{lang} {key}");
        }
    }
}

#[tokio::test]
async fn start_page_has_the_edit_button() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    for (lang, label) in [("en", "Edit"), ("nb", "Rediger")] {
        let res = start_page(&mut web, lang).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        // A kit pill button with the pencil icon and aria-pressed.
        let at = res.body.find(r#"id="mm-edit""#).expect("no edit button");
        let start = res.body[..at].rfind("<button").unwrap();
        let end = res.body[at..].find("</button>").unwrap() + at;
        let button = &res.body[start..end];
        assert!(button.contains(r#"type="button""#), "{button}");
        assert!(button.contains(r#"aria-pressed="false""#), "{button}");
        assert!(button.contains(r#"class="btn btn-secondary""#), "{button}");
        assert!(button.contains("icons.svg#pencil"), "{button}");
        assert!(
            button.contains(&format!("<span>{label}</span>")),
            "{lang}: {button}"
        );
        assert!(!button.contains("autofocus"), "{button}");
        // Without scripts the button would do nothing: it starts hidden, and the
        // page script shows it.
        let wrapper = &res.body[res.body[..start].rfind("<div").unwrap()..start];
        assert!(
            wrapper.contains("start-actions") && wrapper.contains("hidden"),
            "{wrapper}"
        );
    }
}

/// The keys `edit.js` lists in `LABEL_KEYS`.
fn edit_label_keys() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("static/magpie");
    let source = std::fs::read_to_string(dir.join("edit.js")).unwrap();
    let list = source
        .split("export const LABEL_KEYS = [")
        .nth(1)
        .and_then(|rest| rest.split("];").next())
        .expect("edit.js lists no LABEL_KEYS");
    list.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn start_page_labels_cover_every_string_edit_mode_uses() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let keys = edit_label_keys();
    assert!(keys.len() > 60, "only {} keys found", keys.len());
    let mut texts = Vec::new();
    for lang in ["en", "nb"] {
        let res = start_page(&mut web, lang).await;
        let labels = json_block(&res.body, "mm-labels");
        for key in &keys {
            let text = labels[key.as_str()].as_str().unwrap_or_default();
            assert!(
                !text.is_empty() && !text.starts_with("start."),
                "{lang}: no text for {key}"
            );
        }
        texts.push(labels);
    }
    // The two languages really differ (a key copied untranslated would not).
    for key in ["edit_done", "conflict", "reload", "save_failed", "add_card"] {
        assert_ne!(texts[0][key], texts[1][key], "{key} is not translated");
    }
    assert_eq!(texts[1]["conflict"], "Siden er endret i en annen fane");
    assert_eq!(texts[1]["reload"], "Last inn på nytt");
    assert_eq!(texts[1]["save_failed"], "Kunne ikke lagre – prøv igjen");
    assert_eq!(texts[1]["edit_done"], "Ferdig");
    assert_eq!(texts[1]["add_card"], "Legg til kort");
    // A text with a placeholder keeps it for the script to fill in.
    assert!(texts[0]["edit_item"].as_str().unwrap().contains("{title}"));
    assert!(texts[1]["err_too_long"].as_str().unwrap().contains("{max}"));
}

#[tokio::test]
async fn edit_mode_scripts_are_served() {
    let app = App::new().await;
    for path in [
        "/static/magpie/ops.js",
        "/static/magpie/rules.js",
        "/static/magpie/edit.js",
    ] {
        let res = app.get(path).await;
        assert_eq!(res.status, StatusCode::OK, "{path}: {}", res.body);
        assert!(
            res.header("content-type").contains("javascript"),
            "{path} content-type: {}",
            res.header("content-type")
        );
    }
}

#[tokio::test]
async fn start_page_carries_the_signed_in_users_number() {
    // A draft kept in the browser is tied to this number, so that the next
    // person in the same tab never sees it (static/magpie/start.js).
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let mut bob = Web::new(&app, "bob", "viewer").await;
    let ada_id = common::user_id(&app, "ada").await;
    let bob_id = common::user_id(&app, "bob").await;
    assert_ne!(ada_id, bob_id);
    for (web, id, name) in [(&mut ada, ada_id, "ada"), (&mut bob, bob_id, "bob")] {
        let res = start_page(web, "en").await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        let at = res.body.find(r#"id="groups""#).unwrap();
        let tag = &res.body[at..res.body[at..].find('>').unwrap() + at];
        assert!(tag.contains(&format!(r#"data-user="{id}""#)), "{tag}");
        assert!(!tag.contains(name), "the name is not on the page: {tag}");
    }
}

#[tokio::test]
async fn start_page_is_not_kept_in_the_htmx_history_cache() {
    // htmx keeps a snapshot of every page it leaves in sessionStorage, and the
    // snapshot outlives a sign-out in the same tab: Back could show the start
    // page, with its layout, to the next person. `hx-history="false"` anywhere
    // in the page makes htmx keep no snapshot of it; Back then fetches the page
    // from the server, for whoever is signed in.
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let res = start_page(&mut web, "en").await;
    assert!(
        res.body.contains(r#"hx-history="false""#),
        "the start page does not opt out of htmx's history cache"
    );
    // Nor in the browser's own caches: Back after a sign-out asks the server.
    assert_eq!(res.header("cache-control"), "no-store");
    // The page's own data is in the page, not in a snapshot.
    assert!(res.body.contains(r#"id="mm-layout""#), "{}", res.body);

    // The check lives in the htmx the kit ships: it looks for this selector.
    // (If a newer htmx changes it, this fails and the opt-out needs a new look.)
    let htmx = app.get("/static/htmx.min.js").await;
    assert_eq!(htmx.status, StatusCode::OK);
    assert!(
        htmx.body.contains(r#"[hx-history="false" i]"#),
        "this htmx does not know hx-history=false"
    );
}

#[tokio::test]
async fn magpies_own_scripts_and_styles_are_revalidated_not_kept_for_an_hour() {
    // Their URLs carry no version, so after an update a browser must not run
    // the old script against the new server. The kit's ETag stays: an
    // unchanged file is a 304.
    let app = App::new().await;
    for path in [
        "/static/magpie/start.js",
        "/static/magpie/edit.js",
        "/static/magpie/render.js",
        "/static/magpie/render.css",
        "/static/magpie.css",
        "/static/clock.js",
    ] {
        let res = app.get(path).await;
        assert_eq!(res.status, StatusCode::OK, "{path}");
        assert_eq!(res.header("cache-control"), "no-cache", "{path}");
        let etag = res.header("etag").to_string();
        assert!(!etag.is_empty(), "{path} has an ETag");
        let again = app
            .send(
                Request::get(path)
                    .header("if-none-match", &etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(again.status, StatusCode::NOT_MODIFIED, "{path}");
        assert_eq!(again.header("cache-control"), "no-cache", "{path}");
        assert_eq!(again.header("etag"), etag, "{path}");
    }
}

/// The `[detail]` section of a locale file: its keys.
fn detail_keys(lang: &str) -> Vec<String> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("locales/{lang}.toml"));
    let toml = std::fs::read_to_string(path).unwrap();
    let mut inside = false;
    let mut keys = Vec::new();
    for line in toml.lines() {
        if line.starts_with('[') {
            inside = line.trim() == "[detail]";
        } else if inside {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // Nothing in the section may be left unread (a text over two lines,
            // a key with dashes): the extension's generated copy and this test
            // would both miss it.
            let (key, value) = line
                .split_once(" = ")
                .unwrap_or_else(|| panic!("{lang}: cannot read the [detail] line {line:?}"));
            assert!(
                !key.is_empty()
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{lang}: [detail] key {key:?} is not lower-case letters, digits and underscores"
            );
            assert!(
                value.len() >= 2 && value.starts_with('"') && value.ends_with('"'),
                "{lang}: [detail] {key} is not one quoted line"
            );
            keys.push(key.to_string());
        }
    }
    keys
}

/// The keys the kit has under `[detail]` itself.
const KIT_DETAIL_KEYS: [&str; 2] = ["expires", "request"];

/// The short names some kinds' detail keys start with instead of the kind's id.
const KIND_ALIASES: [(&str, &str); 2] = [("proxmox", "pve"), ("github", "gh")];

#[tokio::test]
async fn every_detail_key_starts_with_the_id_of_a_kind() {
    let _app = App::new().await;
    let kinds: Vec<&str> = mindful_magpie::integrations::registry()
        .iter()
        .map(|k| k.kind())
        .collect();
    // An alias belongs to a kind that exists, and is not itself the id of one.
    for (kind, alias) in KIND_ALIASES {
        assert!(kinds.contains(&kind), "{kind} is not a kind");
        assert!(!kinds.contains(&alias), "{alias} is a kind of its own");
    }
    let prefixes: Vec<&str> = kinds
        .iter()
        .copied()
        .chain(KIND_ALIASES.iter().map(|(_, alias)| *alias))
        .collect();
    for key in detail_keys("en") {
        assert!(
            !KIT_DETAIL_KEYS.contains(&key.as_str()),
            "{key} is a key of the kit's"
        );
        assert!(
            prefixes
                .iter()
                .any(|prefix| key.starts_with(&format!("{prefix}_"))),
            "[detail] {key} must start with the id of a kind or its alias and an underscore ({prefixes:?}), so that kinds and the kit do not collide"
        );
    }
}

#[tokio::test]
async fn the_labels_carry_the_block_strings_and_every_detail_key() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    // Both languages name the same detail keys. (The kit has two keys of its own under the same
    // heading, `expires` and `request`; they are carried too, and a kind must not reuse them.)
    let keys = detail_keys("en");
    assert_eq!(keys, detail_keys("nb"));
    for lang in ["en", "nb"] {
        let res = start_page(&mut web, lang).await;
        let labels = json_block(&res.body, "mm-labels");
        let more = labels["block_more"].as_str().unwrap();
        assert!(more.contains("{n}"), "{lang}: {more}");
        for key in &keys {
            let text = labels[format!("detail.{key}").as_str()]
                .as_str()
                .unwrap_or_default();
            assert!(
                !text.is_empty() && !text.starts_with("detail."),
                "{lang}: no text for detail.{key}"
            );
        }
    }
}

#[tokio::test]
async fn a_detail_key_is_known_when_the_locale_has_its_text() {
    use mindful_magpie::integrations::block::{key_known, key_shape};
    let _app = App::new().await;
    for key in detail_keys("en") {
        assert!(key_shape(&key), "{key} is shaped like a key");
        assert!(key_known(&key), "{key} is known");
    }
    assert!(!key_known("no_such_key_in_the_locale"));
    assert!(!key_known("Bad Shape"));
    assert!(!key_known(""));
}
