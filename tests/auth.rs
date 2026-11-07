//! Setup, sign-in, users and roles from vole-kit, as Magpie mounts them: the
//! first-run page, no open registration, who may manage users, and that the
//! accounts survive a restart.

mod common;

use axum::http::StatusCode;
use common::{App, PASSWORD, Web, add_user};
use vole_kit::auth::session;
use vole_kit::db::migrate;

/// Closes `/setup` behind the token for the calling thread and puts the
/// default back on drop, also when the test fails.
struct SetupClosed;

impl SetupClosed {
    fn new() -> SetupClosed {
        session::set_setup_open_for_tests(Some(false));
        SetupClosed
    }
}

impl Drop for SetupClosed {
    fn drop(&mut self) {
        session::set_setup_open_for_tests(None);
    }
}

/// The `/setup` form of the first administrator.
fn setup_form(token: &str) -> Vec<(&str, &str)> {
    vec![
        ("username", "ada"),
        ("password", PASSWORD),
        ("confirm", PASSWORD),
        ("site_name", ""),
        ("timezone", "UTC"),
        ("token", token),
    ]
}

#[tokio::test]
async fn first_run_redirects_to_setup() {
    let app = App::new().await;
    let res = app.get("/").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.header("location"), "/setup");
}

#[tokio::test]
async fn setup_creates_the_first_admin() {
    // The kit's default: /setup needs the setup token.
    let _closed = SetupClosed::new();
    let token = session::required_setup_token().expect("setup needs a token");
    let app = App::new().await;
    let mut web = Web::anonymous(&app);

    // Without the token nothing is created.
    let res = web.post("/setup", &setup_form("")).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert_eq!(app.count("SELECT COUNT(*) FROM users").await, 0);

    let res = web.post("/setup", &setup_form(&token)).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.header("location"), "/");
    assert_eq!(
        app.count("SELECT COUNT(*) FROM users WHERE username = 'ada' AND role = 'admin'")
            .await,
        1
    );
    assert!(web.cookies.contains_key("mm_session"));
    let home = web.get("/").await;
    assert_eq!(home.status, StatusCode::OK, "{}", home.body);
}

#[tokio::test]
async fn no_open_registration() {
    let app = App::new().await;
    add_user(&app, "ada", "admin").await;
    let mut web = Web::anonymous(&app);
    // With a user the setup page is closed: it sends a visitor to sign in.
    let res = web.get("/setup").await;
    assert_ne!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.header("location"), "/login");
    let res = web.post("/setup", &setup_form("")).await;
    assert_ne!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM users").await,
        1,
        "setup created a second user"
    );
    // The start page itself needs a session.
    let res = web.get("/").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(res.header("location").starts_with("/login"));
    assert_eq!(web.get("/register").await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        web.post("/register", &[("username", "eve"), ("password", PASSWORD)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn member_cannot_manage_users() {
    let app = App::new().await;
    add_user(&app, "ada", "admin").await;
    let mut gina = Web::new(&app, "gina", "viewer").await;
    assert_eq!(gina.get("/").await.status, StatusCode::OK);
    let res = gina.get("/settings/users").await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    let res = gina
        .post(
            "/settings/users",
            &[
                ("username", "mallory"),
                ("display_name", ""),
                ("role", "viewer"),
                ("password", PASSWORD),
            ],
        )
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM users WHERE username = 'mallory'")
            .await,
        0
    );
}

#[tokio::test]
async fn admin_can_add_a_user() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let page = ada.get("/settings/users").await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let res = ada
        .post(
            "/settings/users",
            &[
                ("username", "gina"),
                ("display_name", "Gina"),
                ("role", "viewer"),
                ("password", PASSWORD),
            ],
        )
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM users WHERE username = 'gina' AND role = 'viewer'")
            .await,
        1
    );
    let mut gina = Web::anonymous(&app);
    let res = gina
        .post("/login", &[("username", "gina"), ("password", PASSWORD)])
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(gina.get("/").await.status, StatusCode::OK);
}

#[tokio::test]
async fn data_survives_restart() {
    let app = App::new().await;
    add_user(&app, "ada", "admin").await;
    let app = app.restart().await;
    let mut ada = Web::anonymous(&app);
    let res = ada
        .post("/login", &[("username", "ada"), ("password", PASSWORD)])
        .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(ada.get("/").await.status, StatusCode::OK);
    // One row per migration: the kit's core set and Magpie's own, none twice.
    let names = migrate::all_migration_names();
    assert_eq!(
        app.count("SELECT COUNT(*) FROM schema_migrations").await,
        names.len() as i64
    );
    assert_eq!(
        app.count("SELECT COUNT(DISTINCT name) FROM schema_migrations")
            .await,
        names.len() as i64
    );
}

#[tokio::test]
async fn settings_home_goes_to_the_first_tab_the_user_may_open() {
    let app = App::new().await;
    let mut ada = Web::new(&app, "ada", "admin").await;
    let res = ada.get("/settings").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.header("location"), "/settings/general");

    let mut gina = Web::new(&app, "gina", "viewer").await;
    let res = gina.get("/settings").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert_eq!(res.header("location"), "/settings/security");
    assert_eq!(gina.get("/settings/security").await.status, StatusCode::OK);

    let mut anon = Web::anonymous(&app);
    let res = anon.get("/settings").await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    assert!(res.header("location").starts_with("/login"));
}
