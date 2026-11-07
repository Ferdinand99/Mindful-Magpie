//! The skeleton boots: the health probe answers, Magpie's own static files
//! are served, its own migration ran and the kit's API reference renders.

mod common;

use axum::http::StatusCode;
use common::{App, Web};

#[tokio::test]
async fn healthz_answers_ok() {
    let app = App::new().await;
    let res = app.get("/healthz").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

#[tokio::test]
async fn static_css_is_served() {
    let app = App::new().await;
    let res = app.get("/static/magpie.css").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(
        res.header("content-type").starts_with("text/css"),
        "content-type: {}",
        res.header("content-type")
    );
}

#[tokio::test]
async fn own_migration_ran() {
    let app = App::new().await;
    // `0001_init.sql` creates the table; nothing writes to it yet.
    assert_eq!(app.count("SELECT COUNT(*) FROM magpie_meta").await, 0);
}

#[tokio::test]
async fn api_docs_render_for_a_signed_in_admin() {
    let app = App::new().await;
    let mut web = Web::new(&app, "ada", "admin").await;
    let res = web.get("/api-docs").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body.contains("Mindful Magpie"), "{}", res.body);
}
