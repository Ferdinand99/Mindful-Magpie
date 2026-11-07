//! The caps on open `/start/live` streams: 64 in all and 8 for one user. In a
//! file of its own, and one test: the caps are per process, and the streams of
//! other tests would count against them.

mod common;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderValue, Request, StatusCode, header};
use common::{App, HOST, Web};
use mindful_magpie::values::{MAX_STREAMS, MAX_STREAMS_PER_USER};
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

fn assert_busy(res: &axum::response::Response) {
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.headers()[header::RETRY_AFTER], "30");
}

#[tokio::test]
async fn one_user_has_eight_streams_and_sixty_four_are_open_in_all() {
    assert_eq!(MAX_STREAMS_PER_USER, 8);
    assert_eq!(MAX_STREAMS, 64);
    let app = App::new().await;
    let names = ["u0", "u1", "u2", "u3", "u4", "u5", "u6", "u7", "u8"];
    let mut webs = Vec::new();
    for name in names {
        webs.push(Web::new(&app, name, "viewer").await);
    }

    // The first user: eight, and the ninth is told to wait.
    let mut open_streams = Vec::new();
    for n in 0..MAX_STREAMS_PER_USER {
        let res = open(&webs[0]).await;
        assert_eq!(res.status(), StatusCode::OK, "stream {n} of one user");
        open_streams.push(res);
    }
    assert_busy(&open(&webs[0]).await);
    // Another user is not affected by it.
    let other = open(&webs[1]).await;
    assert_eq!(other.status(), StatusCode::OK);
    open_streams.push(other);

    // A closed stream gives its place back to the user.
    drop(open_streams.remove(0));
    let res = open(&webs[0]).await;
    assert_eq!(res.status(), StatusCode::OK);
    open_streams.push(res);

    // Fill the rest of the 64 with the other users' streams.
    for web in &webs[1..8] {
        while open_streams.len() < MAX_STREAMS {
            let res = open(web).await;
            if res.status() != StatusCode::OK {
                assert_busy(&res);
                break;
            }
            open_streams.push(res);
        }
    }
    assert_eq!(open_streams.len(), MAX_STREAMS, "sixty-four are open");
    // The ninth user has none of their own, and the process is full.
    assert_busy(&open(&webs[8]).await);

    // A place given back is the next user's.
    drop(open_streams.pop());
    let res = open(&webs[8]).await;
    assert_eq!(res.status(), StatusCode::OK);
    drop(res);
    drop(open_streams);
}
