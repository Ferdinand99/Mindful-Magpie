//! Cards bound to services: the save checks, the `#mm-services`
//! embed, the extension's API, and what a user who cannot see a service learns
//! about it (nothing).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{App, HOST, Res, Web, add_user, bearer_get, make_token, user_id};
use mindful_magpie::services::{
    SecretChange, ServiceInput, Viewer, Visibility, services_for, store,
};
use serde_json::{Value, json};
use vole_kit::auth::TokenScope;

const SECRET: &str = "s3cr3t-Tok3n-cards-1a2b3c";
const URL_HOST: &str = "10.77.88.99";
const URL: &str = "http://10.77.88.99:8123/health-check-path";
const SETTING: &str = "250-251";

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

fn bound_card(id: &str, service: Option<Value>) -> Value {
    let mut card = json!({
        "id": id,
        "title": "Blog",
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

async fn page(web: &mut Web<'_>, lang: &str) -> Res {
    let req = Request::get("/")
        .header("accept-language", lang)
        .body(Body::empty())
        .unwrap();
    let res = web.send(req).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    res
}

fn names(services: &Value) -> Vec<String> {
    services
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect()
}

fn assert_no_secrets(text: &str, what: &str) {
    for needle in [
        SECRET,
        URL_HOST,
        "health-check-path",
        SETTING,
        "expected_status",
    ] {
        assert!(!text.contains(needle), "{what} leaks {needle:?}");
    }
}

// ---------------------------------------------------------------------------
// Saving

#[tokio::test]
async fn an_admin_binds_a_card_and_the_layout_comes_back_with_it() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    let id = service(&app, "Blog", Visibility::Admin, &[]).await;

    let layout = doc(
        0,
        vec![bound_card("c1", Some(bind(id, &["status", "checked"])))],
    );
    let res = save(&mut admin, &layout).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    let embedded = block(&page(&mut admin, "en").await.body, "mm-layout");
    assert_eq!(
        embedded["groups"][0]["cards"][0]["service"],
        json!({ "id": id, "metrics": ["status", "checked"] })
    );
    // And it can be saved again, bound the same way.
    let again = save(
        &mut admin,
        &doc(
            1,
            vec![bound_card("c1", Some(bind(id, &["status", "checked"])))],
        ),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
}

#[tokio::test]
async fn a_card_may_be_bound_to_one_to_four_metrics_of_a_visible_service() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    let id = service(&app, "Blog", Visibility::Admin, &[]).await;
    for (i, metrics) in [vec!["status"], vec!["status", "latency_ms", "checked"]]
        .into_iter()
        .enumerate()
    {
        let res = save(
            &mut admin,
            &doc(i as u64, vec![bound_card("c1", Some(bind(id, &metrics)))]),
        )
        .await;
        assert_eq!(res.status, StatusCode::OK, "{metrics:?}: {}", res.body);
    }
}

#[tokio::test]
async fn too_many_no_and_repeated_metrics_are_refused_before_the_database() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    let id = service(&app, "Blog", Visibility::Admin, &[]).await;
    for (metrics, code) in [
        (vec!["a", "b", "c", "d", "e"], "too_many"),
        (vec![], "required"),
        (vec!["status", "status"], "duplicate_id"),
        (vec!["bad id!"], "bad_id"),
        (vec![""], "bad_id"),
    ] {
        let res = save(
            &mut admin,
            &doc(0, vec![bound_card("c1", Some(bind(id, &metrics)))]),
        )
        .await;
        assert_eq!(
            res.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{metrics:?}: {}",
            res.body
        );
        assert_eq!(
            res.json()["fields"],
            json!([{ "path": "groups[0].cards[0].service.metrics", "code": code }]),
            "{metrics:?}"
        );
    }
    let long = "x".repeat(41);
    let res = save(
        &mut admin,
        &doc(0, vec![bound_card("c1", Some(bind(id, &[long.as_str()])))]),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    // A service id that cannot be an id.
    let res = save(
        &mut admin,
        &doc(0, vec![bound_card("c1", Some(bind(0, &["status"])))]),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.json()["fields"][0]["code"], "bad_service");
    // Extra fields in the binding are refused like everywhere else.
    let odd = json!({ "id": id, "metrics": ["status"], "name": "x" });
    let res = save(&mut admin, &doc(0, vec![bound_card("c1", Some(odd))])).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    // Nothing was stored by any of them.
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 0);
}

#[tokio::test]
async fn an_unknown_metric_is_bad_metric() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    let id = service(&app, "Blog", Visibility::Admin, &[]).await;
    let res = save(
        &mut admin,
        &doc(
            0,
            vec![bound_card("c1", Some(bind(id, &["status", "nope"])))],
        ),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
    assert_eq!(
        res.json()["fields"],
        json!([{ "path": "groups[0].cards[0].service.metrics", "code": "bad_metric" }])
    );
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 0);
}

#[tokio::test]
async fn a_hidden_service_is_refused_exactly_like_one_that_does_not_exist() {
    let app = App::new().await;
    let _admin = Web::new(&app, "ada", "admin").await;
    let mut vera = Web::new(&app, "vera", "viewer").await;
    let hidden = service(&app, "Hidden-Name-XYZ", Visibility::Admin, &[]).await;
    let missing = hidden + 1000;

    let for_hidden = save(
        &mut vera,
        &doc(0, vec![bound_card("c1", Some(bind(hidden, &["status"])))]),
    )
    .await;
    let for_missing = save(
        &mut vera,
        &doc(0, vec![bound_card("c1", Some(bind(missing, &["status"])))]),
    )
    .await;
    assert_eq!(
        for_hidden.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        for_hidden.body
    );
    assert_eq!(
        for_hidden.json(),
        json!({ "error": "invalid",
                "fields": [{ "path": "groups[0].cards[0].service", "code": "bad_service" }] })
    );
    // The same answer, byte for byte: nothing tells the two apart.
    assert_eq!(for_hidden.body, for_missing.body);
    assert_eq!(for_hidden.status, for_missing.status);
    for res in [&for_hidden, &for_missing] {
        assert!(!res.body.contains("Hidden-Name-XYZ"));
        assert!(!res.body.contains("http"), "the kind leaks: {}", res.body);
        assert_no_secrets(&res.body, "a 422");
    }
    // A hidden service with a metric that does not exist is still bad_service,
    // never bad_metric (that would say the service exists).
    let probe = save(
        &mut vera,
        &doc(0, vec![bound_card("c1", Some(bind(hidden, &["nope"])))]),
    )
    .await;
    assert_eq!(probe.body, for_hidden.body);
    assert_eq!(app.count("SELECT COUNT(*) FROM layouts").await, 0);
}

#[tokio::test]
async fn a_service_for_selected_users_can_be_bound_by_them_only() {
    let app = App::new().await;
    let _admin = Web::new(&app, "ada", "admin").await;
    let mut vera = Web::new(&app, "vera", "viewer").await;
    let mut bob = Web::new(&app, "bob", "viewer").await;
    let vera_id = user_id(&app, "vera").await;
    let id = service(&app, "Veras", Visibility::Users, &[vera_id]).await;
    let layout = doc(0, vec![bound_card("c1", Some(bind(id, &["status"])))]);
    assert_eq!(save(&mut vera, &layout).await.status, StatusCode::OK);
    let res = save(&mut bob, &layout).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(res.json()["fields"][0]["code"], "bad_service");
}

#[tokio::test]
async fn a_card_keeps_a_binding_whose_service_went_away_but_cannot_take_a_new_one() {
    let app = App::new().await;
    let _admin = Web::new(&app, "ada", "admin").await;
    let mut vera = Web::new(&app, "vera", "viewer").await;
    let id = service(&app, "For all", Visibility::All, &[]).await;
    let first = doc(
        0,
        vec![bound_card("c1", Some(bind(id, &["status", "checked"])))],
    );
    assert_eq!(save(&mut vera, &first).await.status, StatusCode::OK);

    // The admin makes it admin-only, then deletes it.
    for step in ["hide", "delete"] {
        if step == "hide" {
            store::update(
                &app.magpie.core,
                id,
                &input("For all", Visibility::Admin, &[]),
            )
            .await
            .unwrap();
        } else {
            assert!(store::delete(&app.magpie.core, id).await.unwrap());
        }
        // Another change on the same page still saves, with the binding as it was.
        let mut next = doc(
            0,
            vec![bound_card("c1", Some(bind(id, &["status", "checked"])))],
        );
        next["version"] = json!(app.count("SELECT version FROM layouts").await);
        next["groups"][0]["cards"][0]["title"] = json!(format!("Blog {step}"));
        let res = save(&mut vera, &next).await;
        assert_eq!(res.status, StatusCode::OK, "{step}: {}", res.body);

        // A different binding to it is a new decision, and is checked.
        let mut other = next.clone();
        other["version"] = json!(app.count("SELECT version FROM layouts").await);
        other["groups"][0]["cards"][0]["service"] = bind(id, &["status"]);
        let res = save(&mut vera, &other).await;
        assert_eq!(
            res.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{step}: {}",
            res.body
        );
        assert_eq!(res.json()["fields"][0]["code"], "bad_service");

        // So is the same binding on a card the layout did not have before.
        let mut copy = next.clone();
        copy["version"] = json!(app.count("SELECT version FROM layouts").await);
        let extra = bound_card("c2", Some(bind(id, &["status", "checked"])));
        copy["groups"][0]["cards"]
            .as_array_mut()
            .unwrap()
            .push(extra);
        let res = save(&mut vera, &copy).await;
        assert_eq!(
            res.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{step}: {}",
            res.body
        );
        assert_eq!(
            res.json()["fields"][0]["path"],
            "groups[0].cards[1].service"
        );
    }
}

#[tokio::test]
async fn a_stored_card_id_with_another_hidden_service_or_another_order_is_fully_checked() {
    let app = App::new().await;
    let _admin = Web::new(&app, "ada", "admin").await;
    let mut vera = Web::new(&app, "vera", "viewer").await;
    let mine = service(&app, "Mine", Visibility::All, &[]).await;
    let hidden = service(&app, "Hidden-Name-XYZ", Visibility::Admin, &[]).await;
    let first = doc(
        0,
        vec![bound_card("c1", Some(bind(mine, &["status", "checked"])))],
    );
    assert_eq!(save(&mut vera, &first).await.status, StatusCode::OK);

    // The same card id, now naming a service the user cannot see.
    let forged = doc(
        1,
        vec![bound_card("c1", Some(bind(hidden, &["status", "checked"])))],
    );
    let res = save(&mut vera, &forged).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
    assert_eq!(res.json()["fields"][0]["code"], "bad_service");
    assert!(!res.body.contains("Hidden-Name-XYZ"));

    // The same service and metrics in another order is another binding: it is
    // checked, so once the service is hidden it is refused.
    store::update(
        &app.magpie.core,
        mine,
        &input("Mine", Visibility::Admin, &[]),
    )
    .await
    .unwrap();
    let reordered = doc(
        1,
        vec![bound_card("c1", Some(bind(mine, &["checked", "status"])))],
    );
    let res = save(&mut vera, &reordered).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", res.body);
    assert_eq!(res.json()["fields"][0]["code"], "bad_service");
    // Exactly as it was is still let through.
    let same = doc(
        1,
        vec![bound_card("c1", Some(bind(mine, &["status", "checked"])))],
    );
    assert_eq!(save(&mut vera, &same).await.status, StatusCode::OK);
}

#[tokio::test]
async fn the_service_check_runs_in_the_version_transaction_a_stale_save_changes_nothing() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    let id = service(&app, "Blog", Visibility::Admin, &[]).await;
    assert_eq!(
        save(
            &mut admin,
            &doc(0, vec![bound_card("c1", Some(bind(id, &["status"])))])
        )
        .await
        .status,
        StatusCode::OK
    );
    // Version 0 again: a conflict, whatever the card says.
    let res = save(
        &mut admin,
        &doc(0, vec![bound_card("c1", Some(bind(id, &["checked"])))]),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    let stored: String = app
        .magpie
        .core
        .with_conn(|c| c.query_row("SELECT doc FROM layouts", [], |r| r.get(0)))
        .await
        .unwrap();
    assert!(stored.contains(r#""metrics":["status"]"#), "{stored}");
}

// ---------------------------------------------------------------------------
// Old layouts

#[tokio::test]
async fn an_old_layout_without_service_loads_and_saves_unchanged() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    let ada = user_id(&app, "ada").await;
    // What a page stored before cards could be bound to services: no `service` anywhere.
    let old = doc(0, vec![bound_card("c1", None)]);
    let stored = serde_json::to_string(&old).unwrap();
    app.magpie
        .core
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO layouts (user_id, version, doc, updated_at) VALUES (?1, 4, ?2, 'x')",
                rusqlite::params![ada, stored],
            )
        })
        .await
        .unwrap();

    let embedded = block(&page(&mut admin, "en").await.body, "mm-layout");
    let mut expected = old.clone();
    expected["version"] = json!(4);
    assert_eq!(embedded, expected, "an unbound card gets no service key");

    let res = save(&mut admin, &embedded).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let after: String = app
        .magpie
        .core
        .with_conn(|c| c.query_row("SELECT doc FROM layouts", [], |r| r.get(0)))
        .await
        .unwrap();
    let mut expected = old;
    expected["version"] = json!(5);
    assert_eq!(serde_json::from_str::<Value>(&after).unwrap(), expected);
    assert!(!after.contains("service"), "{after}");

    // An explicit `null` (what the editor sends for «Ingen») is the same as none.
    let mut with_null = embedded;
    with_null["version"] = json!(5);
    with_null["groups"][0]["cards"][0]["service"] = Value::Null;
    assert_eq!(save(&mut admin, &with_null).await.status, StatusCode::OK);
    let after: String = app
        .magpie
        .core
        .with_conn(|c| c.query_row("SELECT doc FROM layouts", [], |r| r.get(0)))
        .await
        .unwrap();
    assert!(!after.contains("service"), "{after}");
}

// ---------------------------------------------------------------------------
// The embed

#[tokio::test]
async fn the_page_always_embeds_a_services_block() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    // No services at all: an empty list, not a missing block (the renderer
    // reads a missing list as "this page cannot show service numbers").
    let res = page(&mut admin, "en").await;
    assert_eq!(block(&res.body, "mm-services"), json!([]));
}

#[tokio::test]
async fn the_embed_has_only_what_the_user_may_see_and_never_a_secret() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    let mut vera = Web::new(&app, "vera", "viewer").await;
    let mut bob = Web::new(&app, "bob", "viewer").await;
    let vera_id = user_id(&app, "vera").await;
    let all = service(&app, "Visible for all", Visibility::All, &[]).await;
    let admins = service(&app, "Admins-Only-Svc", Visibility::Admin, &[]).await;
    let veras = service(&app, "Just Vera", Visibility::Users, &[vera_id]).await;

    let seen = |res: &Res| block(&res.body, "mm-services");

    let as_admin = page(&mut admin, "en").await;
    let services = seen(&as_admin);
    assert_eq!(
        names(&services),
        ["Admins-Only-Svc", "Just Vera", "Visible for all"]
    );
    // Exactly the documented shape, nothing more.
    let first = &services[0];
    let mut keys: Vec<&str> = first
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["id", "interval_s", "kind", "metrics", "name"]);
    assert_eq!(first["id"], admins);
    assert_eq!(first["kind"], "http");
    assert_eq!(first["interval_s"], 60);
    assert_eq!(
        first["metrics"],
        json!([
            { "id": "status", "label": "HTTP status", "format": "number" },
            { "id": "latency_ms", "label": "Response time (ms)", "format": "number" },
            { "id": "checked", "label": "Last checked", "format": "since" },
        ])
    );

    let as_vera = page(&mut vera, "en").await;
    assert_eq!(names(&seen(&as_vera)), ["Just Vera", "Visible for all"]);
    assert_eq!(seen(&as_vera)[0]["id"], veras);
    assert_eq!(seen(&as_vera)[1]["id"], all);
    let as_bob = page(&mut bob, "en").await;
    assert_eq!(names(&seen(&as_bob)), ["Visible for all"]);

    // Nobody learns more: not the hidden name, nor the address, setting or key.
    for (who, res) in [("admin", &as_admin), ("vera", &as_vera), ("bob", &as_bob)] {
        assert_no_secrets(&res.body, &format!("the page for {who}"));
    }
    assert!(!as_vera.body.contains("Admins-Only-Svc"));
    assert!(!as_bob.body.contains("Admins-Only-Svc"));
    assert!(!as_bob.body.contains("Just Vera"));
}

#[tokio::test]
async fn metric_labels_are_in_the_page_language() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    service(&app, "Blog", Visibility::Admin, &[]).await;
    let en = block(&page(&mut admin, "en").await.body, "mm-services");
    let nb = block(&page(&mut admin, "nb").await.body, "mm-services");
    assert_eq!(en[0]["metrics"][0]["label"], "HTTP status");
    assert_eq!(nb[0]["metrics"][0]["label"], "HTTP-status");
    assert_eq!(nb[0]["metrics"][2]["label"], "Sist sjekket");
    // The format ids do not change with the language.
    assert_eq!(en[0]["metrics"][2]["format"], "since");
    assert_eq!(nb[0]["metrics"][2]["format"], "since");
}

#[tokio::test]
async fn the_services_block_cannot_end_early() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    service(&app, "</script><!-- x", Visibility::Admin, &[]).await;
    let res = page(&mut admin, "en").await;
    assert_eq!(names(&block(&res.body, "mm-services")), ["</script><!-- x"]);
    assert!(!res.body.contains("</script><!--"));
}

#[tokio::test]
async fn services_for_follows_visible_to() {
    let app = App::new().await;
    let core = &app.magpie.core;
    let admin = add_user(&app, "ada", "admin").await;
    let ann = add_user(&app, "ann", "viewer").await;
    service(&app, "For all", Visibility::All, &[]).await;
    service(&app, "Admins", Visibility::Admin, &[]).await;
    let for_admin = services_for(core, Viewer::new(admin, true)).await.unwrap();
    let for_ann = services_for(core, Viewer::new(ann, false)).await.unwrap();
    assert_eq!(for_admin.len(), 2);
    assert_eq!(for_ann.len(), 1);
    assert_eq!(for_ann[0].name, "For all");
    assert_eq!(for_ann[0].metrics.len(), 3);
    assert_eq!(for_ann[0].interval_s, 60);
    let json = serde_json::to_string(&for_ann).unwrap();
    assert_no_secrets(&json, "services_for");
}

// ---------------------------------------------------------------------------
// The extension's API

/// The token the extension's connect flow gives `web`'s user: consent, then
/// the code swapped for a token, as the extension does it.
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
async fn me_start_adds_services_next_to_the_layout_and_keeps_its_shape() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    let all = service(&app, "For all", Visibility::All, &[]).await;
    service(&app, "Admins-Only-Svc", Visibility::Admin, &[]).await;
    let layout = doc(0, vec![bound_card("c1", Some(bind(all, &["status"])))]);
    assert_eq!(save(&mut admin, &layout).await.status, StatusCode::OK);

    let token = extension_token(&app, &mut admin).await;
    let res = bearer_get(&app, "/api/v1/me/start", &token).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let body = res.json();
    // The shape the released extension reads is untouched at the top level.
    assert_eq!(body["version"], 1);
    assert!(body["groups"].is_array() && body["bookmarks"].is_array());
    assert_eq!(body["background"]["name"], "hav");
    assert_eq!(body["groups"][0]["cards"][0]["service"]["id"], all);
    // The admin's own extension token shows the admin's services.
    assert_eq!(names(&body["services"]), ["Admins-Only-Svc", "For all"]);
    assert_eq!(body["services"][1]["metrics"][0]["format"], "number");
    assert_no_secrets(&res.body, "me/start for an admin");
    assert_eq!(res.header("cache-control"), "no-store");
}

#[tokio::test]
async fn an_admins_plain_read_token_is_a_viewer_but_the_extensions_token_is_the_admin() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    service(&app, "For all", Visibility::All, &[]).await;
    service(&app, "Admins-Only-Svc", Visibility::Admin, &[]).await;
    let ada = user_id(&app, "ada").await;

    // A script's read-only token (Tokens page, no product scope): the kit's cap,
    // a viewer, so a leaked one shows no admin-only name.
    let plain = make_token(&app, ada, TokenScope::Read, "script").await;
    let res = bearer_get(&app, "/api/v1/me/start", &plain).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(names(&res.json()["services"]), ["For all"]);
    assert!(!res.body.contains("Admins-Only-Svc"));

    // The token the connect flow gives: read-only plus the owner scope.
    let ext = extension_token(&app, &mut admin).await;
    let res = bearer_get(&app, "/api/v1/me/start", &ext).await;
    assert_eq!(
        names(&res.json()["services"]),
        ["Admins-Only-Svc", "For all"]
    );
    // It is stored as a read token with the scope.
    let (scope, scopes): (String, String) = app
        .magpie
        .core
        .with_conn(|c| {
            c.query_row(
                "SELECT t.scope, group_concat(s.scope) FROM api_tokens t
                 JOIN api_token_scopes s ON s.token_id = t.id
                 WHERE t.name LIKE 'Utvidelse:%'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .await
        .unwrap();
    assert_eq!((scope.as_str(), scopes.as_str()), ("read", "start:owner"));

    // A read-and-write token is the account's own role: the kit's rule, unchanged.
    let write = make_token(&app, ada, TokenScope::Write, "writer").await;
    let res = bearer_get(&app, "/api/v1/me/start", &write).await;
    assert_eq!(
        names(&res.json()["services"]),
        ["Admins-Only-Svc", "For all"]
    );
}

#[tokio::test]
async fn a_viewers_extension_token_never_sees_admin_only_services() {
    let app = App::new().await;
    let _admin = Web::new(&app, "ada", "admin").await;
    let mut vera = Web::new(&app, "vera", "viewer").await;
    let vera_id = user_id(&app, "vera").await;
    service(&app, "For all", Visibility::All, &[]).await;
    service(&app, "Admins-Only-Svc", Visibility::Admin, &[]).await;
    service(&app, "Just Vera", Visibility::Users, &[vera_id]).await;

    let ext = extension_token(&app, &mut vera).await;
    let res = bearer_get(&app, "/api/v1/me/start", &ext).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(names(&res.json()["services"]), ["For all", "Just Vera"]);
    assert!(!res.body.contains("Admins-Only-Svc"));
    assert_no_secrets(&res.body, "me/start for a viewer's extension");

    // A viewer who gives a plain token the scope by hand gains nothing: the
    // scope only lifts the cap, the owner's own role still decides.
    let plain = make_token(&app, vera_id, TokenScope::Read, "mine").await;
    let id: i64 = app
        .magpie
        .core
        .with_conn(|c| {
            c.query_row("SELECT id FROM api_tokens WHERE name = 'mine'", [], |r| {
                r.get(0)
            })
        })
        .await
        .unwrap();
    app.magpie
        .core
        .with_conn(move |c| vole_kit::db::api_tokens::add_scope(c, id, "start:owner"))
        .await
        .unwrap();
    let res = bearer_get(&app, "/api/v1/me/start", &plain).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(!res.body.contains("Admins-Only-Svc"));
}

#[tokio::test]
async fn the_tokens_page_names_the_owner_scope_in_both_languages() {
    let app = App::new().await;
    let mut admin = Web::new(&app, "ada", "admin").await;
    for (lang, label) in [("en", "Read, as you"), ("nb", "Lese, som deg")] {
        let req = Request::get("/settings/tokens")
            .header("accept-language", lang)
            .body(Body::empty())
            .unwrap();
        let res = admin.send(req).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        assert!(
            res.body.contains("start:owner"),
            "{lang}: the choice is offered"
        );
        assert!(res.body.contains(label), "{lang}: {label}");
        assert!(
            !res.body.contains("tokens.scope_owner"),
            "{lang}: a missing key shows as itself"
        );
    }
}

#[tokio::test]
async fn me_start_for_a_card_whose_service_is_hidden_names_nothing() {
    let app = App::new().await;
    let _admin = Web::new(&app, "ada", "admin").await;
    let mut vera = Web::new(&app, "vera", "viewer").await;
    let id = service(&app, "Soon-Hidden-Name", Visibility::All, &[]).await;
    let layout = doc(0, vec![bound_card("c1", Some(bind(id, &["status"])))]);
    assert_eq!(save(&mut vera, &layout).await.status, StatusCode::OK);
    store::update(
        &app.magpie.core,
        id,
        &input("Soon-Hidden-Name", Visibility::Admin, &[]),
    )
    .await
    .unwrap();

    let vera_id = user_id(&app, "vera").await;
    let token = make_token(&app, vera_id, TokenScope::Read, "vera-ext").await;
    let res = bearer_get(&app, "/api/v1/me/start", &token).await;
    assert!(!res.body.contains("Soon-Hidden-Name"), "{}", res.body);
    assert_eq!(res.json()["services"], json!([]));
    // The card still points at the id, which the renderer shows as «gone».
    assert_eq!(res.json()["groups"][0]["cards"][0]["service"]["id"], id);
    let embedded = page(&mut vera, "en").await;
    assert!(!embedded.body.contains("Soon-Hidden-Name"));
}
