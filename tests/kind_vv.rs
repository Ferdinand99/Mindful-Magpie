//! The `vv` kind («Vigilant Vole») against a stub server on 127.0.0.1: the
//! summary, an old server (404, then `/health`), a refused key, answers that
//! are not what was expected, the needs, and that the token goes nowhere but
//! into the request.

use std::io::Write;
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use mindful_magpie::integrations::{
    self, FetchCtx, FetchError, Integration, Level, Snapshot, Status,
};
use serde_json::{Value as Json, json};

const TOKEN: &str = "vv-T0ken-Zq81x7w4mK";

// ---------------------------------------------------------------------------
// Capturing what is logged

#[derive(Clone)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn log_buffer() -> &'static LogBuffer {
    static BUFFER: OnceLock<LogBuffer> = OnceLock::new();
    static INSTALL: Once = Once::new();
    let buffer = BUFFER.get_or_init(|| LogBuffer(Arc::new(Mutex::new(Vec::new()))));
    INSTALL.call_once(|| {
        let writer = buffer.clone();
        // Also bridges the `log` records of reqwest and hyper.
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .try_init()
            .expect("no other subscriber in this test binary");
    });
    buffer
}

fn logged() -> String {
    String::from_utf8_lossy(&log_buffer().0.lock().unwrap()).into_owned()
}

// ---------------------------------------------------------------------------
// The stub

/// What a path answers.
#[derive(Clone)]
enum Answer {
    Json(Json),
    Raw(&'static str),
    Status(u16),
    /// Never answers.
    Hang,
}

/// One request the stub saw.
#[derive(Clone, Debug)]
struct Hit {
    path: String,
    query: Option<String>,
    authorization: Option<String>,
}

#[derive(Clone)]
struct Stub {
    summary: Answer,
    health: Answer,
    hits: Arc<Mutex<Vec<Hit>>>,
}

impl Stub {
    fn hits_on(&self, path: &str) -> Vec<Hit> {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|h| h.path == path)
            .cloned()
            .collect()
    }
}

const SUMMARY: &str = "/api/v1/summary";
const HEALTH: &str = "/api/v1/health";

async fn answer(State(stub): State<Stub>, uri: Uri, headers: HeaderMap) -> Response {
    stub.hits.lock().unwrap().push(Hit {
        path: uri.path().to_string(),
        query: uri.query().map(str::to_string),
        authorization: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
    });
    let what = match uri.path() {
        SUMMARY => stub.summary.clone(),
        HEALTH => stub.health.clone(),
        _ => Answer::Status(404),
    };
    match what {
        Answer::Json(v) => axum::Json(v).into_response(),
        Answer::Raw(text) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            text,
        )
            .into_response(),
        Answer::Status(code) => StatusCode::from_u16(code).unwrap().into_response(),
        Answer::Hang => {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            StatusCode::OK.into_response()
        }
    }
}

/// Starts a stub on a free port; its address (no trailing slash) and handle.
async fn serve(summary: Answer, health: Answer) -> (String, Stub) {
    let stub = Stub {
        summary,
        health,
        hits: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new().fallback(answer).with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), stub)
}

fn kind() -> &'static dyn Integration {
    integrations::find("vv").expect("the vv kind is registered")
}

fn ctx(url: &str, token: Option<&str>) -> FetchCtx {
    FetchCtx {
        http: integrations::client().clone(),
        settings: json!({ "url": url }),
        secret: token.map(str::to_string),
    }
}

/// A fetch with the token; the kind answers with a snapshot.
async fn fetch(url: &str) -> Snapshot {
    kind()
        .fetch(&ctx(url, Some(TOKEN)))
        .await
        .expect("the vv kind answers with a snapshot")
}

fn summary(up: u64, down: u64, pending: u64, unknown: u64, paused: u64) -> Json {
    json!({
        "version": "0.17.0",
        "monitors": {
            "total": up + down + pending + unknown + paused,
            "up": up, "down": down, "pending": pending, "maintenance": 0,
            "paused": paused, "unknown": unknown,
        },
        "uptime": { "days": 30, "pct": 99.87, "pct_24h": 100.0, "monitors_counted": up },
        "down": [],
    })
}

fn num(s: &Snapshot, id: &str) -> Option<f64> {
    s.values.get(id).unwrap_or_else(|| panic!("no {id}")).num
}

// ---------------------------------------------------------------------------
// The kind

#[test]
fn the_kind_describes_itself() {
    let k = kind();
    assert_eq!(k.kind(), "vv");
    assert_eq!(k.label_key(), "kind.vv");
    assert_eq!(k.default_interval(), Duration::from_secs(30));
    assert_eq!(k.min_interval(), Duration::from_secs(15));
    assert!(k.url_required());
    let ids: Vec<_> = k.metrics().iter().map(|m| m.id).collect();
    for id in ["up", "down", "uptime_30d", "uptime_24h", "paused"] {
        assert!(ids.contains(&id), "{id}");
    }
    let fields = k.fields();
    assert_eq!(fields.len(), 1);
    assert!(fields[0].secret && fields[0].required);
    // Every text it needs is in both languages.
    let mut keys: Vec<&str> = vec![k.label_key(), fields[0].label_key, fields[0].help_key];
    keys.extend(k.metrics().iter().map(|m| m.label_key));
    keys.extend(["need.vv_down", "fetch_err.auth"]);
    for (lang, text) in [
        ("en", include_str!("../locales/en.toml")),
        ("nb", include_str!("../locales/nb.toml")),
    ] {
        for key in &keys {
            let (section, name) = key.split_once('.').unwrap();
            assert!(has_text(text, section, name), "{lang}: {key}");
        }
    }
}

/// Whether `[section]` of a locale file has a non-empty text called `name`.
fn has_text(locale: &str, section: &str, name: &str) -> bool {
    let mut inside = false;
    for line in locale.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == format!("[{section}]");
        } else if inside
            && let Some(value) = line
                .strip_prefix(name)
                .and_then(|r| r.trim().strip_prefix('='))
        {
            return value.trim().len() > 2;
        }
    }
    false
}

#[tokio::test]
async fn a_full_summary_becomes_values_and_status() {
    let mut body = summary(40, 2, 1, 0, 3);
    body["down"] = json!([
        { "id": 7, "name": "Blog", "since": "2026-11-01T10:00:00Z", "message": "timeout" },
        { "id": 9, "name": "Shop", "since": "2026-11-02T10:00:00Z", "message": null },
    ]);
    let (url, stub) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;

    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error, None);
    assert_eq!(num(&s, "up"), Some(40.0));
    assert_eq!(num(&s, "down"), Some(2.0));
    assert_eq!(num(&s, "paused"), Some(3.0));
    assert_eq!(num(&s, "uptime_30d"), Some(99.87));
    assert_eq!(num(&s, "uptime_24h"), Some(100.0));
    assert_eq!(s.values["first_down"].text.as_deref(), Some("Blog"));

    // One call, 30 days, with the token as a bearer.
    let hits = stub.hits_on(SUMMARY);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].query.as_deref(), Some("days=30"));
    assert_eq!(
        hits[0].authorization.as_deref(),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert!(stub.hits_on(HEALTH).is_empty());
}

#[tokio::test]
async fn the_status_follows_down_and_pending() {
    for (body, want) in [
        (summary(5, 0, 0, 0, 2), Status::Ok),
        (summary(5, 0, 1, 0, 0), Status::Warn),
        (summary(5, 0, 0, 1, 0), Status::Warn),
        (summary(5, 1, 1, 1, 0), Status::Bad),
        (summary(0, 0, 0, 0, 0), Status::Ok),
    ] {
        let (url, _) = serve(Answer::Json(body.clone()), Answer::Status(500)).await;
        assert_eq!(fetch(&url).await.status, want, "{body}");
    }
}

#[tokio::test]
async fn a_null_percent_is_a_value_without_a_number() {
    let mut body = summary(0, 0, 0, 0, 4);
    body["uptime"] = json!({ "days": 30, "pct": null, "pct_24h": null, "monitors_counted": 0 });
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Ok);
    assert_eq!(num(&s, "uptime_30d"), None);
    assert_eq!(num(&s, "uptime_24h"), None);
    assert_eq!(s.values["uptime_30d"].text, None);
    assert_eq!(num(&s, "paused"), Some(4.0));
    assert_eq!(num(&s, "up"), Some(0.0));
    // One percent known, the other not.
    let mut body = summary(3, 0, 0, 0, 0);
    body["uptime"]["pct_24h"] = Json::Null;
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;
    assert_eq!(num(&s, "uptime_30d"), Some(99.87));
    assert_eq!(num(&s, "uptime_24h"), None);
}

#[tokio::test]
async fn fields_it_does_not_know_are_ignored_and_the_optional_ones_may_be_missing() {
    let body = json!({
        "monitors": { "up": 2, "down": 0, "something_new": 5 },
        "future": [1, 2, 3],
    });
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Ok);
    assert_eq!(num(&s, "up"), Some(2.0));
    assert_eq!(num(&s, "uptime_30d"), None);
}

// ---------------------------------------------------------------------------
// Needs

#[tokio::test]
async fn down_monitors_make_one_need_with_the_count_as_score() {
    let mut body = summary(10, 3, 0, 0, 0);
    body["down"] = json!([{ "id": 1, "name": "Blog", "since": null, "message": "x" }]);
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;
    assert_eq!(s.needs.len(), 1);
    let need = &s.needs[0];
    assert_eq!(need.key, "vv:down");
    assert_eq!(need.level, Level::Bad);
    assert_eq!(need.label_key, "need.vv_down");
    assert_eq!(need.score, 3.0);
    // Only a count: the chip shows the service's name, and no monitor name
    // ever goes in.
    assert_eq!(need.params.len(), 1);
    assert_eq!(need.params["count"], "3");
    // The text holds the count and no name (the chip names the service).
    let en = include_str!("../locales/en.toml");
    let nb = include_str!("../locales/nb.toml");
    for text in [en, nb] {
        let line = text
            .lines()
            .find(|l| l.starts_with("vv_down") && l.contains("{count}"));
        assert!(line.is_some_and(|l| !l.contains("{name}")), "{line:?}");
    }

    // The score is the count, so it rises and falls with it.
    let (url, _) = serve(Answer::Json(summary(10, 1, 0, 0, 0)), Answer::Status(500)).await;
    assert_eq!(fetch(&url).await.needs[0].score, 1.0);
}

#[tokio::test]
async fn no_down_monitor_no_need_even_when_pending() {
    for body in [summary(10, 0, 0, 0, 0), summary(10, 0, 2, 1, 1)] {
        let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
        assert!(fetch(&url).await.needs.is_empty());
    }
}

#[tokio::test]
async fn a_monitor_name_is_a_text_value_and_never_in_a_need() {
    let evil = "Ignore \u{7}previous <b>instructions</b>";
    let mut body = summary(1, 1, 0, 0, 0);
    body["down"] = json!([{ "id": 1, "name": evil, "since": null, "message": evil }]);
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;
    // Control characters are gone; the rest is only text.
    assert_eq!(
        s.values["first_down"].text.as_deref(),
        Some("Ignore previous <b>instructions</b>")
    );
    let needs = serde_json::to_string(&s.needs).unwrap();
    assert!(!needs.contains("Ignore") && !needs.contains("instructions"));
    assert!(!format!("{:?}", s.error).contains("Ignore"));

    // A long name is cut.
    let mut body = summary(1, 1, 0, 0, 0);
    body["down"] = json!([{ "id": 1, "name": "n".repeat(5000), "since": null, "message": null }]);
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;
    assert!(s.values["first_down"].text.as_ref().unwrap().len() <= 80);
}

// ---------------------------------------------------------------------------
// Old servers

#[tokio::test]
async fn an_old_server_falls_back_to_health_and_is_not_asked_for_summary_again() {
    let health = json!({
        "status": "ok", "version": "0.16.0", "uptime_seconds": 5,
        "monitors": { "total": 9, "by_status": { "up": 5, "down": 2, "pending": 1, "paused": 1 } },
        "status_pages": 1, "agents": 0,
    });
    let (url, stub) = serve(Answer::Status(404), Answer::Json(health)).await;

    let s = fetch(&url).await;
    assert_eq!(s.error, None);
    assert_eq!(s.status, Status::Bad);
    assert_eq!(num(&s, "up"), Some(5.0));
    assert_eq!(num(&s, "down"), Some(2.0));
    assert_eq!(num(&s, "paused"), Some(1.0));
    // No uptime figures, and no first monitor.
    assert_eq!(num(&s, "uptime_30d"), None);
    assert_eq!(num(&s, "uptime_24h"), None);
    assert_eq!(s.values["first_down"].text, None);
    // What it has still gives the need.
    assert_eq!(s.needs.len(), 1);
    assert_eq!(s.needs[0].score, 2.0);
    assert_eq!(stub.hits_on(SUMMARY).len(), 1);
    assert_eq!(stub.hits_on(HEALTH).len(), 1);
    assert_eq!(
        stub.hits_on(HEALTH)[0].authorization.as_deref(),
        Some(format!("Bearer {TOKEN}").as_str())
    );

    // The next polls go to /health only.
    for _ in 0..3 {
        let s = fetch(&url).await;
        assert_eq!(num(&s, "up"), Some(5.0));
    }
    assert_eq!(stub.hits_on(SUMMARY).len(), 1, "summary is not asked again");
    assert_eq!(stub.hits_on(HEALTH).len(), 4);
}

#[tokio::test]
async fn a_server_without_either_endpoint_is_a_bad_reading_not_an_error() {
    let (url, _) = serve(Answer::Status(404), Answer::Status(404)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("HTTP 404"));
    assert!(s.values.is_empty() && s.needs.is_empty());
}

#[tokio::test]
async fn a_health_answer_with_no_counts_is_zero_counts() {
    let health = json!({ "monitors": { "total": 0, "by_status": {} } });
    let (url, _) = serve(Answer::Status(404), Answer::Json(health)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Ok);
    assert_eq!(num(&s, "up"), Some(0.0));
}

// ---------------------------------------------------------------------------
// A refused key, and everything else that goes wrong

#[tokio::test]
async fn a_refused_key_is_a_bad_reading_with_the_auth_text() {
    for code in [401, 403] {
        let (url, stub) = serve(Answer::Status(code), Answer::Status(code)).await;
        let s = fetch(&url).await;
        assert_eq!(s.status, Status::Bad, "{code}");
        let text = s.error.as_deref().unwrap();
        assert_eq!(text, FetchError::Auth.to_string());
        assert_eq!(integrations::error_text_key(text), Some("fetch_err.auth"));
        assert!(s.values.is_empty() && s.needs.is_empty());
        assert!(!text.contains(TOKEN) && !text.contains("Bearer"));
        // It does not go on to /health with a key that was refused.
        assert!(stub.hits_on(HEALTH).is_empty());
    }
}

#[tokio::test]
async fn a_key_refused_by_health_on_an_old_server_is_refused_too() {
    let (url, _) = serve(Answer::Status(404), Answer::Status(401)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("key refused"));
}

#[tokio::test]
async fn malformed_answers_are_bad_readings_and_do_not_panic() {
    for raw in [
        "",
        "not json",
        "{",
        "[]",
        "null",
        "{}",
        r#"{"monitors": 5}"#,
        r#"{"monitors": {"up": -1}}"#,
        r#"{"monitors": {"up": "many"}}"#,
        r#"{"monitors": {"up": 1}, "down": 7}"#,
        r#"{"monitors": {"up": 1}, "down": [5]}"#,
        "\u{0}\u{1}\u{2}",
    ] {
        let (url, _) = serve(Answer::Raw(raw), Answer::Status(500)).await;
        let s = fetch(&url).await;
        assert_eq!(s.status, Status::Bad, "{raw:?}");
        assert_eq!(s.error.as_deref(), Some("unexpected answer"), "{raw:?}");
        assert!(s.values.is_empty() && s.needs.is_empty(), "{raw:?}");
    }
    // The same from /health.
    let (url, _) = serve(Answer::Status(404), Answer::Raw("<html>")).await;
    let s = fetch(&url).await;
    assert_eq!(s.error.as_deref(), Some("unexpected answer"));
}

#[tokio::test]
async fn server_errors_and_a_refused_connection_are_bad_readings() {
    let (url, _) = serve(Answer::Status(500), Answer::Status(500)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("HTTP 500"));

    // A port nothing listens on.
    let free = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let s = fetch(&format!("http://{free}")).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("connection failed"));
}

#[tokio::test]
async fn a_huge_answer_is_cut_off() {
    let (url, _) = serve(
        Answer::Json(json!({ "monitors": { "up": 1 }, "pad": "x".repeat(2 * 1024 * 1024) })),
        Answer::Status(500),
    )
    .await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("response too large"));
}

#[tokio::test]
async fn a_server_that_never_answers_times_out() {
    let (url, _) = serve(Answer::Hang, Answer::Hang).await;
    let http = integrations::build_client(Duration::from_millis(400));
    let c = FetchCtx {
        http,
        settings: json!({ "url": url }),
        secret: Some(TOKEN.into()),
    };
    let s = kind().fetch(&c).await.unwrap();
    assert_eq!(s.status, Status::Bad);
    assert_eq!(s.error.as_deref(), Some("timeout"));
}

#[tokio::test]
async fn settings_it_cannot_use_are_errors_that_name_no_value() {
    let (url, stub) = serve(Answer::Json(summary(1, 0, 0, 0, 0)), Answer::Status(500)).await;
    // No token.
    for token in [None, Some("")] {
        let err = kind().fetch(&ctx(&url, token)).await.unwrap_err();
        assert_eq!(err, FetchError::Config);
    }
    // A token that cannot go into a header.
    let err = kind()
        .fetch(&ctx(&url, Some("bad\ntoken")))
        .await
        .unwrap_err();
    assert_eq!(err, FetchError::Config);
    // Addresses that are no good.
    for bad in [
        "",
        "not a url",
        "ftp://example.com/",
        "http://user:pw@127.0.0.1:1/",
    ] {
        let err = kind().fetch(&ctx(bad, Some(TOKEN))).await.unwrap_err();
        assert_eq!(err, FetchError::Config, "{bad:?}");
        assert!(!err.to_string().contains("pw"));
    }
    // The metadata address is refused before anything is sent.
    let err = kind()
        .fetch(&ctx("http://169.254.169.254/", Some(TOKEN)))
        .await
        .unwrap_err();
    assert_eq!(err, FetchError::Forbidden);
    // No address in the settings at all.
    let c = FetchCtx {
        http: integrations::client().clone(),
        settings: json!({}),
        secret: Some(TOKEN.into()),
    };
    assert_eq!(kind().fetch(&c).await.unwrap_err(), FetchError::Config);
    assert!(stub.hits.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_path_in_the_address_is_kept() {
    // Vigilant Vole behind a proxy prefix.
    let (url, stub) = serve(Answer::Status(404), Answer::Status(404)).await;
    let _ = fetch(&format!("{url}/vole/")).await;
    let paths: Vec<_> = stub
        .hits
        .lock()
        .unwrap()
        .iter()
        .map(|h| h.path.clone())
        .collect();
    assert_eq!(paths, ["/vole/api/v1/summary", "/vole/api/v1/health"]);
}

// ---------------------------------------------------------------------------
// The token

#[tokio::test]
async fn the_token_is_in_the_request_only() {
    let _ = log_buffer();
    let mut snapshots = Vec::new();
    let mut errors = Vec::new();

    let mut body = summary(3, 1, 0, 0, 0);
    body["down"] = json!([{ "id": 1, "name": "Blog", "since": null, "message": null }]);
    let cases = [
        (Answer::Json(body), Answer::Status(500)),
        (Answer::Status(401), Answer::Status(401)),
        (Answer::Status(403), Answer::Status(403)),
        (
            Answer::Status(404),
            Answer::Json(json!({ "monitors": { "by_status": { "up": 1 } } })),
        ),
        (Answer::Raw("garbage"), Answer::Status(500)),
        (Answer::Status(500), Answer::Status(500)),
    ];
    for (summary, health) in cases {
        let (url, _) = serve(summary, health).await;
        let c = ctx(&url, Some(TOKEN));
        // What the poller keeps and what a page would show.
        match kind().fetch(&c).await {
            Ok(s) => snapshots.push(s),
            Err(e) => errors.push(e),
        }
        let _ = format!("{c:?}");
    }
    // An address nothing listens on.
    snapshots.push(fetch("http://127.0.0.1:1").await);
    assert_eq!(snapshots.len(), 7);

    let mut all = String::new();
    for s in &snapshots {
        all.push_str(&format!("{s:?}"));
        all.push_str(&serde_json::to_string(s).unwrap());
    }
    for e in &errors {
        all.push_str(&format!("{e} {e:?}"));
    }
    assert!(!all.contains(TOKEN), "a snapshot holds the token");
    assert!(!all.contains("Bearer"), "a snapshot mentions the header");

    let log = logged();
    assert!(
        log.contains("hyper_util") || log.contains("reqwest"),
        "the capture sees the HTTP client's own lines"
    );
    assert!(!log.contains(TOKEN), "the log holds the token");
    assert!(!log.contains("Bearer"), "the log mentions the header");
}

#[tokio::test]
async fn a_redirect_to_another_host_does_not_carry_the_token() {
    // The target is a second stub on another port; reqwest drops the header
    // on a cross-origin redirect, and the client's redirects are checked.
    let (target, target_stub) =
        serve(Answer::Json(summary(4, 0, 0, 0, 0)), Answer::Status(500)).await;
    let location = format!("{target}{SUMMARY}?days=30");
    let app = Router::new().fallback(move || {
        let location = location.clone();
        async move { axum::response::Redirect::temporary(&location) }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let s = fetch(&origin).await;
    assert_eq!(s.status, Status::Ok);
    assert_eq!(num(&s, "up"), Some(4.0));
    let hits = target_stub.hits_on(SUMMARY);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].authorization, None, "the token stays with its host");
}
