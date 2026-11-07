//! The cap on open `/start/live` streams. In a file of its own: the cap is
//! per process, and the streams of the other tests would count against it.

mod common;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderValue, Request, StatusCode, header};
use common::{App, HOST, Web};
use tower::ServiceExt as _;

async fn open(web: &Web<'_>) -> axum::response::Response {
    let mut req = Request::get("/start/live")
        .header(header::HOST, HOST)
        .body(Body::empty())
        .unwrap();
    req.headers_mut().insert(
        header::COOKIE,
        HeaderValue::from_str(&web.cookie_header()).unwrap(),
    );
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:40002".parse::<std::net::SocketAddr>().unwrap(),
    ));
    web.app.router.clone().oneshot(req).await.unwrap()
}

#[tokio::test]
async fn sixty_four_streams_are_open_and_the_next_is_told_to_wait() {
    let app = App::new().await;
    let web = Web::new(&app, "ada", "admin").await;

    let mut open_streams = Vec::new();
    for n in 0..mindful_magpie::values::MAX_STREAMS {
        let res = open(&web).await;
        assert_eq!(res.status(), StatusCode::OK, "stream {n}");
        open_streams.push(res);
    }
    let refused = open(&web).await;
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(refused.headers()[header::RETRY_AFTER], "30");

    // A closed stream gives its place back.
    drop(open_streams.pop());
    let res = open(&web).await;
    assert_eq!(res.status(), StatusCode::OK);
    drop(res);
    drop(open_streams);
}
