//! The skeleton boots: the health probe answers and Magpie's own static files
//! are served.

mod common;

use axum::http::StatusCode;
use common::App;

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
