use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

struct AppState {
    token: String,
    counter: AtomicU64,
}

type Shared = Arc<AppState>;

fn snapshot(state: &AppState) -> Value {
    let counter = state.counter.fetch_add(1, Ordering::SeqCst) + 1;
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default();
    json!({
        "generated_at": now,
        "counter": counter,
        "items": [
            { "label": "Spike", "value": "Mindful Magpie new-tab test" },
            { "label": "Server", "value": "magpie-spike-server" },
            { "label": "Counter", "value": counter.to_string() },
        ],
    })
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(given) = value.strip_prefix("Bearer ") else {
        return false;
    };
    given.as_bytes().ct_eq(token.as_bytes()).into()
}

async fn data(State(state): State<Shared>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.token) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" }))).into_response();
    }
    Json(snapshot(&state)).into_response()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn index(State(state): State<Shared>) -> Html<String> {
    let v = snapshot(&state);
    let mut rows = String::new();
    if let Some(items) = v["items"].as_array() {
        for item in items {
            rows.push_str(&format!(
                "<li><b>{}</b> {}</li>",
                esc(item["label"].as_str().unwrap_or("")),
                esc(item["value"].as_str().unwrap_or(""))
            ));
        }
    }
    Html(format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Magpie spike (web page)</title></head>\
         <body style=\"font-family:system-ui,sans-serif;padding:2rem\">\
         <h1>Magpie spike (plain web page)</h1>\
         <p>generated_at {}, counter {}</p><ul>{}</ul></body></html>",
        esc(v["generated_at"].as_str().unwrap_or("")),
        v["counter"],
        rows
    ))
}

async fn healthz() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() {
    let token = std::env::var("MM_SPIKE_TOKEN").unwrap_or_default();
    if token.is_empty() {
        eprintln!("MM_SPIKE_TOKEN must be set");
        std::process::exit(1);
    }
    let port = std::env::var("MM_SPIKE_PORT").unwrap_or_else(|_| "8099".into());
    let state = Arc::new(AppState {
        token,
        counter: AtomicU64::new(0),
    });
    let app = Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/api/spike/data", get(data))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("bind");
    println!("magpie-spike-server listening on 0.0.0.0:{port}");
    axum::serve(listener, app).await.expect("serve");
}
