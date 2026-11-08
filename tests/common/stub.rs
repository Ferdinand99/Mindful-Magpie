//! A stub of a service's API on 127.0.0.1 for the kind tests: routes by path,
//! every request recorded, and an address of its own per stub (the kinds
//! remember things per address, so tests must not share one).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value as Json;

/// What a path answers.
#[derive(Clone)]
pub enum Answer {
    /// 200 with this JSON.
    Json(Json),
    /// 200 with this text, as JSON (it need not parse).
    Raw(&'static str),
    /// This status and nothing else.
    Status(u16),
    /// This status and this JSON.
    Reply(u16, Json),
    /// Never answers.
    Hang,
    /// A 307 to this path on the same stub (the method and body are kept).
    Moved(&'static str),
    /// Decided per request.
    With(Arc<dyn Fn(&Hit) -> Answer + Send + Sync>),
}

/// One request the stub saw (the path without the stub's own prefix).
#[derive(Clone, Debug)]
pub struct Hit {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub authorization: Option<String>,
    pub api_key: Option<String>,
    pub content_type: Option<String>,
    pub body: String,
}

#[derive(Clone)]
pub struct Stub {
    prefix: String,
    routes: Arc<Mutex<HashMap<String, Answer>>>,
    seen: Arc<Mutex<Vec<Hit>>>,
}

impl Stub {
    /// Makes `path` answer `answer`.
    pub fn route(&self, path: &str, answer: Answer) {
        self.routes.lock().unwrap().insert(path.to_string(), answer);
    }

    /// Makes `path` answer 200 with `body`.
    pub fn ok(&self, path: &str, body: &Json) {
        self.route(path, Answer::Json(body.clone()));
    }

    /// Makes `path` answer 404 again.
    pub fn remove(&self, path: &str) {
        self.routes.lock().unwrap().remove(path);
    }

    /// Every request so far.
    pub fn seen(&self) -> Vec<Hit> {
        self.seen.lock().unwrap().clone()
    }

    /// The requests to `path`.
    pub fn hits_on(&self, path: &str) -> Vec<Hit> {
        self.seen().into_iter().filter(|h| h.path == path).collect()
    }

    /// How many requests went to `path`.
    pub fn count(&self, path: &str) -> usize {
        self.hits_on(path).len()
    }

    pub fn forget_seen(&self) {
        self.seen.lock().unwrap().clear();
    }
}

fn text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

async fn handle(
    State(stub): State<Stub>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri
        .path()
        .strip_prefix(&stub.prefix)
        .unwrap_or_else(|| uri.path())
        .to_string();
    let hit = Hit {
        method: method.to_string(),
        path: path.clone(),
        query: uri.query().map(str::to_string),
        authorization: text(&headers, "authorization"),
        api_key: text(&headers, "x-api-key"),
        content_type: text(&headers, "content-type"),
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    stub.seen.lock().unwrap().push(hit.clone());
    let mut answer = stub
        .routes
        .lock()
        .unwrap()
        .get(&path)
        .cloned()
        .unwrap_or(Answer::Status(404));
    while let Answer::With(decide) = answer {
        answer = decide(&hit);
    }
    match answer {
        Answer::Json(v) => axum::Json(v).into_response(),
        Answer::Raw(text) => ([(header::CONTENT_TYPE, "application/json")], text).into_response(),
        Answer::Status(code) => StatusCode::from_u16(code).unwrap().into_response(),
        Answer::Reply(code, v) => {
            (StatusCode::from_u16(code).unwrap(), axum::Json(v)).into_response()
        }
        Answer::Moved(to) => (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, format!("{}{to}", stub.prefix))],
        )
            .into_response(),
        Answer::Hang => {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            StatusCode::OK.into_response()
        }
        Answer::With(_) => unreachable!("decided above"),
    }
}

/// Starts a stub with no routes (everything is 404 until [`Stub::route`]).
/// The address has no trailing slash and a path of its own.
pub async fn serve() -> (String, Stub) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let stub = Stub {
        prefix: format!("/stub-{}", N.fetch_add(1, Ordering::Relaxed)),
        routes: Arc::default(),
        seen: Arc::default(),
    };
    let app = Router::new().fallback(handle).with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}{}", listener.local_addr().unwrap(), stub.prefix);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, stub)
}

/// A stub with one route.
pub async fn serve_one(path: &str, answer: Answer) -> (String, Stub) {
    let (base, stub) = serve().await;
    stub.route(path, answer);
    (base, stub)
}

/// A server on a port of its own that answers every request with a redirect
/// to `to` (built by `redirect`: `Redirect::temporary`, `::permanent`, `::to`).
pub async fn redirector(redirect: fn(&str) -> axum::response::Redirect, to: String) -> String {
    let app = Router::new().fallback(move || {
        let to = to.clone();
        async move { redirect(&to) }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// The three redirects a client meets (the method kept, or turned to GET).
pub fn redirects() -> [fn(&str) -> axum::response::Redirect; 3] {
    [
        axum::response::Redirect::temporary,
        axum::response::Redirect::permanent,
        axum::response::Redirect::to,
    ]
}
