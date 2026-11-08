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
use jiff::{SignedDuration, Timestamp};
use mindful_magpie::integrations::block::{self, Block};
use mindful_magpie::integrations::{
    self, FetchCtx, FetchError, Integration, Level, Snapshot, Status,
};
use serde_json::{Value as Json, json};

mod common;

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
    /// The path the stub lives under (`/stub-N`), taken off every request.
    prefix: String,
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
    let path = uri
        .path()
        .strip_prefix(&stub.prefix)
        .unwrap_or_else(|| uri.path())
        .to_string();
    stub.hits.lock().unwrap().push(Hit {
        path: path.clone(),
        query: uri.query().map(str::to_string),
        authorization: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
    });
    let what = match path.as_str() {
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

// Each stub lives under a path of its own (`/stub-N`), so no address is ever
// the same as an earlier test's: the kinds remember a 404 and the like per
// address, and the operating system hands a freed port to a later stub.
/// Starts a stub on a free port; its address (no trailing slash) and handle.
async fn serve(summary: Answer, health: Answer) -> (String, Stub) {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let stub = Stub {
        prefix: format!(
            "/stub-{}",
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ),
        summary,
        health,
        hits: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new().fallback(answer).with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}{}", stub.prefix), stub)
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

/// A fetch with the token that is expected to give a reading.
async fn fetch(url: &str) -> Snapshot {
    kind()
        .fetch(&ctx(url, Some(TOKEN)))
        .await
        .expect("the vv kind gives a reading")
}

/// A fetch with the token that is expected to give none: the reason.
async fn fetch_err(url: &str) -> FetchError {
    kind()
        .fetch(&ctx(url, Some(TOKEN)))
        .await
        .expect_err("the vv kind gives no reading")
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

/// A metric's number; `None` when the metric is left out.
fn num(s: &Snapshot, id: &str) -> Option<f64> {
    s.values.get(id).and_then(|v| v.num)
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
    assert_eq!(fields[0].key, "secret");
    assert_eq!(fields[0].label_key, "field.vv_secret");
    assert_eq!(fields[0].placeholder, "vv_…");
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
async fn a_null_percent_leaves_the_metric_out() {
    let mut body = summary(0, 0, 0, 0, 4);
    body["uptime"] = json!({ "days": 30, "pct": null, "pct_24h": null, "monitors_counted": 0 });
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Ok);
    assert!(!s.values.contains_key("uptime_30d"));
    assert!(!s.values.contains_key("uptime_24h"));
    assert_eq!(num(&s, "paused"), Some(4.0));
    assert_eq!(num(&s, "up"), Some(0.0));
    // One percent known, the other not.
    let mut body = summary(3, 0, 0, 0, 0);
    body["uptime"]["pct_24h"] = Json::Null;
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;
    assert_eq!(num(&s, "uptime_30d"), Some(99.87));
    assert!(!s.values.contains_key("uptime_24h"));
    // No empty value is ever stored.
    assert!(
        s.values
            .values()
            .all(|v| v.num.is_some() || v.text.is_some())
    );
}

#[tokio::test]
async fn a_null_uptime_or_down_list_costs_only_those_metrics() {
    let mut body = summary(6, 1, 0, 0, 0);
    body["uptime"] = Json::Null;
    body["down"] = Json::Null;
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Bad);
    assert_eq!(num(&s, "up"), Some(6.0));
    assert_eq!(num(&s, "down"), Some(1.0));
    assert_eq!(s.needs.len(), 1);
    for id in ["uptime_30d", "uptime_24h", "first_down"] {
        assert!(!s.values.contains_key(id), "{id}");
    }
    // A null count: that metric goes, and the status goes on what is left.
    let mut body = summary(6, 0, 1, 0, 0);
    body["monitors"]["down"] = Json::Null;
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;
    assert_eq!(s.status, Status::Warn);
    assert!(!s.values.contains_key("down"));
    assert_eq!(num(&s, "up"), Some(6.0));
}

#[tokio::test]
async fn a_percentage_is_kept_between_0_and_100() {
    let mut body = summary(1, 0, 0, 0, 0);
    body["uptime"] = json!({ "pct": 187.5, "pct_24h": -3.0 });
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;
    assert_eq!(num(&s, "uptime_30d"), Some(100.0));
    assert_eq!(num(&s, "uptime_24h"), Some(0.0));
    // Not a number at all: left out.
    let mut body = summary(1, 0, 0, 0, 0);
    body["uptime"] = json!({ "pct": "high", "pct_24h": [1] });
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let s = fetch(&url).await;
    assert!(!s.values.contains_key("uptime_30d"));
    assert!(!s.values.contains_key("uptime_24h"));
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
    assert!(!s.values.contains_key("uptime_30d"));
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
    // No uptime figures, and no first monitor: the metrics are left out.
    for id in ["uptime_30d", "uptime_24h", "first_down"] {
        assert!(!s.values.contains_key(id), "{id}");
    }
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
async fn a_server_without_either_endpoint_gives_no_reading() {
    let (url, _) = serve(Answer::Status(404), Answer::Status(404)).await;
    assert_eq!(fetch_err(&url).await, FetchError::Status(404));
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
async fn a_refused_key_gives_no_reading_and_the_auth_text() {
    for code in [401, 403] {
        let (url, stub) = serve(Answer::Status(code), Answer::Status(code)).await;
        let err = fetch_err(&url).await;
        assert_eq!(err, FetchError::Auth, "{code}");
        let text = err.to_string();
        assert_eq!(text, "key refused");
        assert_eq!(integrations::error_text_key(&text), Some("fetch_err.auth"));
        assert!(!text.contains(TOKEN) && !text.contains("Bearer"));
        // It does not go on to /health with a key that was refused.
        assert!(stub.hits_on(HEALTH).is_empty());
    }
}

#[tokio::test]
async fn a_key_refused_by_health_on_an_old_server_is_refused_too() {
    let (url, _) = serve(Answer::Status(404), Answer::Status(401)).await;
    assert_eq!(fetch_err(&url).await, FetchError::Auth);
}

#[tokio::test]
async fn malformed_answers_give_no_reading_and_do_not_panic() {
    for raw in [
        "",
        "not json",
        "{",
        "[]",
        "null",
        "{}",
        r#"{"monitors": 5}"#,
        r#"{"monitors": null}"#,
        "\u{0}\u{1}\u{2}",
    ] {
        let (url, _) = serve(Answer::Raw(raw), Answer::Status(500)).await;
        assert_eq!(fetch_err(&url).await, FetchError::Invalid, "{raw:?}");
    }
    // The same from /health.
    let (url, _) = serve(Answer::Status(404), Answer::Raw("<html>")).await;
    assert_eq!(fetch_err(&url).await, FetchError::Invalid);
}

#[tokio::test]
async fn odd_fields_in_a_good_answer_only_drop_their_metric() {
    for (raw, gone) in [
        (r#"{"monitors": {"up": -1, "down": 2}}"#, "up"),
        (r#"{"monitors": {"up": "many", "down": 2}}"#, "up"),
        (
            r#"{"monitors": {"up": 1, "down": 2}, "down": 7}"#,
            "first_down",
        ),
        (
            r#"{"monitors": {"up": 1, "down": 2}, "down": [5]}"#,
            "first_down",
        ),
        (
            r#"{"monitors": {"up": 1, "down": 2}, "uptime": 5}"#,
            "uptime_30d",
        ),
    ] {
        let (url, _) = serve(Answer::Raw(raw), Answer::Status(500)).await;
        let s = fetch(&url).await;
        assert!(!s.values.contains_key(gone), "{raw}");
        assert_eq!(num(&s, "down"), Some(2.0), "{raw}");
        assert_eq!(s.status, Status::Bad, "{raw}");
    }
}

#[tokio::test]
async fn server_errors_and_a_refused_connection_give_no_reading() {
    let (url, _) = serve(Answer::Status(500), Answer::Status(500)).await;
    assert_eq!(fetch_err(&url).await, FetchError::Status(500));

    // A port nothing listens on.
    let free = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    assert_eq!(
        fetch_err(&format!("http://{free}")).await,
        FetchError::Connect
    );
}

#[tokio::test]
async fn a_huge_answer_is_cut_off() {
    let (url, _) = serve(
        Answer::Json(json!({ "monitors": { "up": 1 }, "pad": "x".repeat(2 * 1024 * 1024) })),
        Answer::Status(500),
    )
    .await;
    assert_eq!(fetch_err(&url).await, FetchError::TooLarge);
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
    assert_eq!(kind().fetch(&c).await.unwrap_err(), FetchError::Timeout);
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
    let _ = fetch_err(&format!("{url}/vole/")).await;
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
    errors.push(fetch_err("http://127.0.0.1:1").await);
    assert_eq!(snapshots.len() + errors.len(), 7);

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
        log.contains("reqwest::") || log.contains("hyper_util::client"),
        "the capture sees the HTTP client's own lines"
    );
    assert!(!log.contains(TOKEN), "the log holds the token");
    assert!(!log.contains("Bearer"), "the log mentions the header");
}

#[tokio::test]
async fn a_redirect_to_another_origin_is_refused_and_never_carries_the_token() {
    // The target is a second stub on another port. The client would drop
    // `Authorization` there, but Magpie refuses the redirect, so the other
    // origin sees no request at all.
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

    assert_eq!(fetch_err(&origin).await, FetchError::RedirectScheme);
    assert!(target_stub.hits.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Details

/// `minutes` ago, as Vigilant Vole writes a time (RFC 3339, UTC).
fn minutes_ago(minutes: i64) -> String {
    Timestamp::now()
        .checked_sub(SignedDuration::from_mins(minutes))
        .unwrap()
        .to_string()
}

fn down_monitor(id: i64, name: &str, message: Option<&str>, minutes: i64) -> Json {
    json!({ "id": id, "name": name, "since": minutes_ago(minutes), "message": message })
}

fn with_down(list: Vec<Json>) -> Json {
    let mut body = summary(5, list.len() as u64, 0, 0, 0);
    body["down"] = Json::Array(list);
    body
}

/// What the poller stores: the details cut down, and every key known.
fn stored(mut snapshot: Snapshot) -> Vec<Block> {
    // The locale lookup needs the kit's configuration.
    static KIT: std::sync::Once = std::sync::Once::new();
    KIT.call_once(mindful_magpie::install);
    snapshot.bound_details();
    for block in &snapshot.details {
        for key in block.keys() {
            assert!(block::key_known(key), "no text for detail.{key}");
        }
    }
    snapshot.details
}

fn list<'a>(blocks: &'a [Block], title: &str) -> Option<&'a [block::ListItem]> {
    blocks.iter().find_map(|b| match b {
        Block::List { title_key, items } if title_key == title => Some(items.as_slice()),
        _ => None,
    })
}

#[tokio::test]
async fn the_down_list_has_name_message_age_and_a_bad_status() {
    let body = with_down(vec![
        down_monitor(7, "Sylo dashboard", Some("timeout"), 12),
        down_monitor(9, "Website", None, 3 * 60 + 5),
    ]);
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let blocks = stored(fetch(&url).await);
    let items = list(&blocks, "vv_down_now").expect("the list");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].text, "Sylo dashboard");
    assert_eq!(items[0].sub.as_deref(), Some("timeout"));
    assert_eq!(items[0].status, Some(Status::Bad));
    assert_eq!(items[0].right.as_deref(), Some("12 min"));
    assert_eq!(items[0].href, None);
    assert_eq!(items[1].sub, None);
    assert_eq!(items[1].right.as_deref(), Some("3 h"));
    // Since when, for the page to count from; the text stays for an older page.
    let at = items[0].at.expect("a moment");
    assert!(
        (jiff::Timestamp::now().as_second() - at - 12 * 60).abs() < 120,
        "{at}"
    );
    // Nothing has recovered yet. The strip of the last day comes after the
    // list.
    assert!(list(&blocks, "vv_recovered").is_none());
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[1], last_day());
}

#[tokio::test]
async fn the_down_list_is_bounded_at_eight() {
    let many = (0..25)
        .map(|i| down_monitor(i, &format!("m{i}"), Some("x"), 1))
        .collect();
    let (url, _) = serve(Answer::Json(with_down(many)), Answer::Status(500)).await;
    let blocks = stored(fetch(&url).await);
    let items = list(&blocks, "vv_down_now").expect("the list");
    assert_eq!(items.len(), block::MAX_ITEMS);
    assert_eq!(items[0].text, "m0");
}

#[tokio::test]
async fn hostile_monitor_text_stays_text() {
    let evil = format!(
        "<script>alert(1)</script>\u{202E}evil\u{7}{}",
        "z".repeat(500)
    );
    let body = with_down(vec![down_monitor(1, &evil, Some(&evil), 1)]);
    let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
    let snapshot = fetch(&url).await;
    // Not in the needs, nor in the error.
    let outside = serde_json::to_string(&(&snapshot.needs, &snapshot.error)).unwrap();
    assert!(!outside.contains("script"), "{outside}");
    let blocks = stored(snapshot);
    let items = list(&blocks, "vv_down_now").expect("the list");
    for text in [&items[0].text, items[0].sub.as_ref().unwrap()] {
        assert!(text.starts_with("<script>alert(1)</script>"), "{text}");
        assert!(!text.contains(['\u{202E}', '\u{7}']), "{text}");
        assert!(text.chars().count() <= block::TEXT_CHARS);
    }
}

/// «Last 24 hours»: the strip the page draws from its history of `down`.
fn last_day() -> Block {
    Block::Spark {
        title_key: "vv_last_24h".into(),
        metric: "down".into(),
        style: block::SparkStyle::UpDown,
        daily: false,
    }
}

#[tokio::test]
async fn an_old_server_without_a_down_list_gets_no_list() {
    // A summary with no `down`, or a null one: no list, only the strip of the
    // count (Magpie keeps its history itself).
    for strip in [true, false] {
        let mut body = summary(5, 1, 0, 0, 0);
        if strip {
            body.as_object_mut().unwrap().remove("down");
        } else {
            body["down"] = Json::Null;
        }
        let (url, _) = serve(Answer::Json(body), Answer::Status(500)).await;
        assert_eq!(stored(fetch(&url).await), vec![last_day()]);
    }
    // And a server that has only `/health`.
    let health =
        json!({ "status": "ok", "monitors": { "total": 3, "by_status": { "up": 2, "down": 1 } } });
    let (url, _) = serve(Answer::Status(404), Answer::Json(health)).await;
    let snapshot = fetch(&url).await;
    assert_eq!(snapshot.values["down"].num, Some(1.0));
    assert_eq!(stored(snapshot), vec![last_day()]);
}

#[tokio::test]
async fn monitors_that_left_the_list_are_recovered_until_the_service_is_saved() {
    use common::stub::{Answer as Reply, serve as serve_stub};
    let (base, stub) = serve_stub().await;
    let down = |list: Vec<Json>| stub.route(SUMMARY, Reply::Json(with_down(list)));

    down(vec![
        down_monitor(1, "Alpha", Some("timeout"), 20),
        down_monitor(2, "Beta", None, 5),
    ]);
    let first = stored(fetch(&base).await);
    assert!(
        list(&first, "vv_recovered").is_none(),
        "not before it was seen down"
    );

    // Alpha comes back.
    down(vec![down_monitor(2, "Beta", None, 6)]);
    let second = stored(fetch(&base).await);
    let recovered = list(&second, "vv_recovered").expect("Alpha recovered");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].text, "Alpha");
    assert_eq!(recovered[0].status, Some(Status::Ok));
    assert_eq!(recovered[0].right.as_deref(), Some("0 s"));
    let at = recovered[0].at.expect("when it was seen back");
    assert!(
        (jiff::Timestamp::now().as_second() - at).abs() < 120,
        "{at}"
    );
    // It is still remembered at the next fetch, and Beta is still down.
    let third = stored(fetch(&base).await);
    assert_eq!(list(&third, "vv_recovered").unwrap()[0].text, "Alpha");
    assert_eq!(list(&third, "vv_down_now").unwrap()[0].text, "Beta");

    // Alpha goes down again: no longer «recovered».
    down(vec![
        down_monitor(1, "Alpha", None, 0),
        down_monitor(2, "Beta", None, 7),
    ]);
    let again = stored(fetch(&base).await);
    assert!(list(&again, "vv_recovered").is_none());

    // Beta comes back, then the service is saved: the memory goes.
    down(vec![down_monitor(1, "Alpha", None, 1)]);
    assert!(list(&stored(fetch(&base).await), "vv_recovered").is_some());
    integrations::api::forget_all_for(&base);
    down(vec![down_monitor(1, "Alpha", None, 2)]);
    assert!(list(&stored(fetch(&base).await), "vv_recovered").is_none());
}

#[tokio::test]
async fn a_cut_short_list_does_not_make_monitors_recovered() {
    use common::stub::serve as serve_stub;
    let (base, stub) = serve_stub().await;
    let named = |n: i64| {
        (1..=n)
            .map(|i| down_monitor(i, &format!("m{i}"), None, 1))
            .collect::<Vec<_>>()
    };
    stub.ok(SUMMARY, &with_down(named(3)));
    stored(fetch(&base).await);
    // 30 are down but the list names only the first two (the summary caps it).
    let mut body = with_down(named(2));
    body["monitors"]["down"] = json!(30);
    stub.ok(SUMMARY, &body);
    let blocks = stored(fetch(&base).await);
    assert!(list(&blocks, "vv_recovered").is_none());
}
