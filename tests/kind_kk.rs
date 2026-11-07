//! The `kk` kind (Keen Kestrel): its summary read against a stub server.
//!
//! The kind is called directly (`Integration::fetch` with a `FetchCtx`); the
//! poller and the pages are tested elsewhere. Each test has its own stub
//! server, so the kind's memory of an old server (kept per address) never
//! crosses from one test to another.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use mindful_magpie::integrations::{
    self, FetchCtx, FetchError, Integration, Level, Snapshot, Status,
};
use serde_json::{Value as Json, json};

const TOKEN: &str = "kk-read-token-8f3a91c2d7";

// ---------------------------------------------------------------------------
// The stub server

/// What the stub saw and what it answers.
struct Stub {
    status: StatusCode,
    body: String,
    hits: AtomicUsize,
    seen: Mutex<Vec<(String, String)>>,
    auth: Mutex<Vec<String>>,
}

type Shared = Arc<Stub>;

async fn answer(State(stub): State<Shared>, uri: Uri, headers: HeaderMap) -> Response {
    stub.hits.fetch_add(1, Ordering::SeqCst);
    stub.seen.lock().unwrap().push((
        uri.path().to_string(),
        uri.query().unwrap_or("").to_string(),
    ));
    stub.auth.lock().unwrap().push(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
    );
    (
        stub.status,
        [(header::CONTENT_TYPE, "application/json")],
        stub.body.clone(),
    )
        .into_response()
}

/// Serves `body` with `status` for any path. Returns the base address and the
/// stub's record.
async fn serve(status: u16, body: impl Into<String>) -> (String, Shared) {
    let stub = Arc::new(Stub {
        status: StatusCode::from_u16(status).unwrap(),
        body: body.into(),
        hits: AtomicUsize::new(0),
        seen: Mutex::new(Vec::new()),
        auth: Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .route("/", get(answer))
        .fallback(get(answer))
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), stub)
}

fn ctx(url: &str, secret: Option<&str>) -> FetchCtx {
    FetchCtx {
        http: integrations::client().clone(),
        settings: json!({ "url": url }),
        secret: secret.map(str::to_string),
    }
}

fn kk() -> &'static dyn Integration {
    integrations::find("kk").expect("the kk kind is registered")
}

async fn fetch(url: &str, secret: Option<&str>) -> Result<Snapshot, FetchError> {
    kk().fetch(&ctx(url, secret)).await
}

// ---------------------------------------------------------------------------
// Summaries

fn project(id: i64, newest: Json) -> Json {
    json!({
        "id": id, "slug": format!("p{id}"), "name": format!("Project {id}"), "platform": "rust",
        "new_issues": 0, "regressed_issues": 0, "unresolved_issues": 0,
        "crash_free_sessions": null, "sessions": 0,
        "visitors_today": null, "app_users_today": null,
        "newest_issue": newest,
    })
}

fn issue(id: i64, title: &str, first_seen: &str) -> Json {
    json!({
        "id": id, "title": title, "level": "error", "substatus": "new",
        "first_seen": first_seen, "last_seen": first_seen, "events": 3,
    })
}

fn summary(totals: Json, projects: Vec<Json>) -> String {
    json!({ "hours": 24, "totals": totals, "projects": projects }).to_string()
}

fn totals(new: i64, regressed: i64, unresolved: i64) -> Json {
    json!({
        "projects": 1, "new_issues": new, "regressed_issues": regressed,
        "unresolved_issues": unresolved, "crash_free_sessions": 99.5, "sessions": 400,
        "visitors_today": 120, "app_users_today": 33,
    })
}

fn num(snapshot: &Snapshot, id: &str) -> Option<f64> {
    snapshot.values.get(id).and_then(|v| v.num)
}

fn text<'a>(snapshot: &'a Snapshot, id: &str) -> Option<&'a str> {
    snapshot.values.get(id).and_then(|v| v.text.as_deref())
}

#[tokio::test]
async fn a_full_summary_gives_every_number_and_the_newest_issue() {
    let body = summary(
        totals(2, 0, 9),
        vec![
            project(1, issue(5, "Old failure", "2026-11-01 08:00:00")),
            project(2, issue(9, "Newer failure", "2026-11-06 21:30:00")),
        ],
    );
    let (url, stub) = serve(200, body).await;
    let snapshot = fetch(&url, Some(TOKEN)).await.unwrap();

    assert_eq!(snapshot.status, Status::Warn);
    assert_eq!(snapshot.error, None);
    assert_eq!(num(&snapshot, "new_issues"), Some(2.0));
    assert_eq!(num(&snapshot, "regressed_issues"), Some(0.0));
    assert_eq!(num(&snapshot, "unresolved_issues"), Some(9.0));
    assert_eq!(num(&snapshot, "crash_free"), Some(99.5));
    assert_eq!(num(&snapshot, "visitors_today"), Some(120.0));
    assert_eq!(num(&snapshot, "app_users_today"), Some(33.0));
    assert_eq!(text(&snapshot, "newest_issue"), Some("Newer failure"));
    // Every value is one of the kind's metrics, and the reverse.
    let ids: Vec<&str> = kk().metrics().iter().map(|m| m.id).collect();
    for key in snapshot.values.keys() {
        assert!(ids.contains(&key.as_str()), "{key}");
    }

    // One call, to the summary, for 24 hours, with the token as a bearer.
    assert_eq!(stub.hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        stub.seen.lock().unwrap().as_slice(),
        [("/api/v1/summary".to_string(), "hours=24".to_string())]
    );
    assert_eq!(
        stub.auth.lock().unwrap().as_slice(),
        [format!("Bearer {TOKEN}")]
    );
}

#[tokio::test]
async fn the_status_is_bad_for_regressions_warn_for_new_issues_and_otherwise_ok() {
    for (new, regressed, unresolved, status) in [
        (0, 0, 4, Status::Ok),
        (3, 0, 4, Status::Warn),
        (0, 1, 4, Status::Bad),
        (5, 2, 9, Status::Bad),
    ] {
        let (url, _) = serve(200, summary(totals(new, regressed, unresolved), vec![])).await;
        let snapshot = fetch(&url, Some(TOKEN)).await.unwrap();
        assert_eq!(snapshot.status, status, "{new} new, {regressed} regressed");
    }
}

#[tokio::test]
async fn null_totals_leave_those_numbers_out() {
    let body = summary(
        json!({
            "projects": 0, "new_issues": 0, "regressed_issues": 0, "unresolved_issues": 0,
            "crash_free_sessions": null, "sessions": 0,
            "visitors_today": null, "app_users_today": null,
        }),
        vec![project(1, Json::Null)],
    );
    let (url, _) = serve(200, body).await;
    let snapshot = fetch(&url, Some(TOKEN)).await.unwrap();

    assert_eq!(snapshot.status, Status::Ok);
    assert_eq!(num(&snapshot, "new_issues"), Some(0.0));
    assert_eq!(num(&snapshot, "unresolved_issues"), Some(0.0));
    for absent in [
        "crash_free",
        "visitors_today",
        "app_users_today",
        "newest_issue",
    ] {
        assert!(!snapshot.values.contains_key(absent), "{absent}");
    }
    assert!(snapshot.needs.is_empty());
}

#[tokio::test]
async fn a_zero_is_a_number_not_a_missing_one() {
    let mut t = totals(0, 0, 0);
    t["visitors_today"] = json!(0);
    t["crash_free_sessions"] = json!(0);
    let (url, _) = serve(200, summary(t, vec![])).await;
    let snapshot = fetch(&url, Some(TOKEN)).await.unwrap();
    assert_eq!(num(&snapshot, "visitors_today"), Some(0.0));
    assert_eq!(num(&snapshot, "crash_free"), Some(0.0));
}

#[tokio::test]
async fn needs_carry_the_count_as_score_and_nothing_from_outside() {
    let hostile = "<script>alert(1)</script> {name} secret-title";
    let body = summary(
        totals(4, 3, 11),
        vec![project(1, issue(7, hostile, "2026-11-06 21:30:00"))],
    );
    let (url, _) = serve(200, body).await;
    let snapshot = fetch(&url, Some(TOKEN)).await.unwrap();

    assert_eq!(snapshot.status, Status::Bad);
    let by_key = |key: &str| snapshot.needs.iter().find(|n| n.key == key).unwrap();
    assert_eq!(snapshot.needs.len(), 2);

    let regressed = by_key("kk:regressed");
    assert_eq!(regressed.level, Level::Bad);
    assert_eq!(regressed.label_key, "need.kk_regressed");
    assert_eq!(regressed.score, 3.0);
    assert_eq!(regressed.params.get("count").map(String::as_str), Some("3"));

    let new = by_key("kk:new");
    assert_eq!(new.level, Level::Warn);
    assert_eq!(new.label_key, "need.kk_new");
    assert_eq!(new.score, 4.0);
    assert_eq!(new.params.get("count").map(String::as_str), Some("4"));

    // Only fixed params; the issue title is in no need.
    for need in &snapshot.needs {
        assert!(
            need.params.keys().map(String::as_str).eq(["count"]),
            "{:?}",
            need.params
        );
        assert!(!format!("{need:?}").contains("script"));
        assert!(!format!("{need:?}").contains("secret-title"));
    }
    // External text is only in the text metric.
    assert_eq!(text(&snapshot, "newest_issue"), Some(hostile));
}

#[tokio::test]
async fn only_regressions_or_only_new_issues_give_only_that_need() {
    let (url, _) = serve(200, summary(totals(0, 2, 5), vec![])).await;
    let snapshot = fetch(&url, Some(TOKEN)).await.unwrap();
    let keys: Vec<&str> = snapshot.needs.iter().map(|n| n.key.as_str()).collect();
    assert_eq!(keys, ["kk:regressed"]);

    let (url, _) = serve(200, summary(totals(1, 0, 5), vec![])).await;
    let snapshot = fetch(&url, Some(TOKEN)).await.unwrap();
    let keys: Vec<&str> = snapshot.needs.iter().map(|n| n.key.as_str()).collect();
    assert_eq!(keys, ["kk:new"]);
}

#[tokio::test]
async fn the_newest_issue_title_is_one_clean_short_line() {
    let long = "x".repeat(500);
    let body = summary(
        totals(1, 0, 1),
        vec![project(
            1,
            issue(
                1,
                &format!("line one\n\tline\u{0}two {long}"),
                "2026-11-06 21:30:00",
            ),
        )],
    );
    let (url, _) = serve(200, body).await;
    let snapshot = fetch(&url, Some(TOKEN)).await.unwrap();
    let title = text(&snapshot, "newest_issue").unwrap();
    assert!(title.starts_with("line one"), "{title:?}");
    assert!(!title.chars().any(char::is_control), "{title:?}");
    assert!(title.chars().count() <= 200, "{}", title.chars().count());
}

#[tokio::test]
async fn the_newest_issue_comes_from_the_project_that_saw_one_last() {
    // Same time: the higher id wins, so the answer does not depend on the order.
    let body = summary(
        totals(0, 0, 3),
        vec![
            project(1, issue(4, "Lower id", "2026-11-06 21:30:00")),
            project(2, issue(8, "Higher id", "2026-11-06 21:30:00")),
            project(3, Json::Null),
        ],
    );
    let (url, _) = serve(200, body).await;
    let snapshot = fetch(&url, Some(TOKEN)).await.unwrap();
    assert_eq!(text(&snapshot, "newest_issue"), Some("Higher id"));
}

// ---------------------------------------------------------------------------
// Failures

#[tokio::test]
async fn an_old_server_is_bad_with_a_closed_error_and_is_asked_once_an_hour() {
    let (url, stub) = serve(404, r#"{"error":"not found"}"#).await;

    let first = fetch(&url, Some(TOKEN)).await.unwrap_err();
    assert_eq!(first, FetchError::KkTooOld);
    assert_eq!(first.text_key(), Some("fetch_err.kk_too_old"));
    assert_eq!(
        integrations::error_text_key(&first.to_string()),
        Some("fetch_err.kk_too_old")
    );
    assert_eq!(stub.hits.load(Ordering::SeqCst), 1);

    // The next fetches (the poller's, the Test button's) do not call again.
    for _ in 0..3 {
        assert_eq!(
            fetch(&url, Some(TOKEN)).await.unwrap_err(),
            FetchError::KkTooOld
        );
    }
    assert_eq!(stub.hits.load(Ordering::SeqCst), 1);

    // Another address has its own memory.
    let (other, other_stub) = serve(404, "").await;
    assert_eq!(
        fetch(&other, Some(TOKEN)).await.unwrap_err(),
        FetchError::KkTooOld
    );
    assert_eq!(other_stub.hits.load(Ordering::SeqCst), 1);
    assert_eq!(stub.hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_refused_key_is_bad_with_the_auth_error() {
    for status in [401, 403] {
        let (url, stub) = serve(status, r#"{"error":"nope"}"#).await;
        let err = fetch(&url, Some(TOKEN)).await.unwrap_err();
        assert_eq!(err, FetchError::Auth, "{status}");
        assert_eq!(err.text_key(), Some("fetch_err.auth"));
        assert_eq!(
            integrations::error_text_key(&err.to_string()),
            Some("fetch_err.auth")
        );
        // Not remembered: a corrected key works at the next fetch.
        let _ = fetch(&url, Some(TOKEN)).await;
        assert_eq!(stub.hits.load(Ordering::SeqCst), 2, "{status}");
    }
}

#[tokio::test]
async fn an_answer_that_is_not_a_summary_is_an_unexpected_answer() {
    for body in [
        "not json at all",
        "",
        "[]",
        "{}",
        r#"{"totals": 5}"#,
        r#"{"totals": {"new_issues": 1}}"#,
        r#"{"totals": {"new_issues": "many", "regressed_issues": 0, "unresolved_issues": 0}}"#,
        r#"{"totals": {"new_issues": -1, "regressed_issues": 0, "unresolved_issues": 0}}"#,
        r#"{"totals": {"new_issues": 1.5, "regressed_issues": 0, "unresolved_issues": 0}}"#,
    ] {
        let (url, _) = serve(200, body).await;
        assert_eq!(
            fetch(&url, Some(TOKEN)).await.unwrap_err(),
            FetchError::Invalid,
            "{body:?}"
        );
    }
}

#[tokio::test]
async fn other_statuses_are_reported_as_they_are() {
    for status in [422, 500, 503] {
        let (url, _) = serve(status, "boom").await;
        assert_eq!(
            fetch(&url, Some(TOKEN)).await.unwrap_err(),
            FetchError::Status(status)
        );
    }
}

#[tokio::test]
async fn an_unreachable_server_is_a_connection_error() {
    // Nothing listens on a port that was just free.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let err = fetch(&format!("http://127.0.0.1:{port}"), Some(TOKEN))
        .await
        .unwrap_err();
    assert_eq!(err, FetchError::Connect);
}

#[tokio::test]
async fn settings_that_cannot_be_used_are_refused_before_anything_is_sent() {
    let (url, stub) = serve(200, summary(totals(0, 0, 0), vec![])).await;
    // No token.
    assert_eq!(fetch(&url, None).await.unwrap_err(), FetchError::Config);
    assert_eq!(fetch(&url, Some("")).await.unwrap_err(), FetchError::Config);
    // A token that is not a header value.
    assert_eq!(
        fetch(&url, Some("bad\ntoken")).await.unwrap_err(),
        FetchError::Config
    );
    // An address with a user name or password, another scheme, or none.
    for bad in [
        "http://user:pw@127.0.0.1:1/",
        "ftp://127.0.0.1/",
        "not a url",
        "",
    ] {
        assert_eq!(
            fetch(bad, Some(TOKEN)).await.unwrap_err(),
            FetchError::Config,
            "{bad:?}"
        );
    }
    let no_url = FetchCtx {
        http: integrations::client().clone(),
        settings: json!({}),
        secret: Some(TOKEN.into()),
    };
    assert_eq!(kk().fetch(&no_url).await.unwrap_err(), FetchError::Config);
    // A cloud metadata address.
    assert_eq!(
        fetch("http://169.254.169.254/", Some(TOKEN))
            .await
            .unwrap_err(),
        FetchError::Forbidden
    );
    assert_eq!(stub.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_address_with_a_path_keeps_it() {
    let (url, stub) = serve(200, summary(totals(0, 0, 0), vec![])).await;
    for base in [
        format!("{url}/kk"),
        format!("{url}/kk/"),
        format!("{url}/?x=1"),
    ] {
        fetch(&base, Some(TOKEN)).await.unwrap();
    }
    let seen = stub.seen.lock().unwrap().clone();
    assert_eq!(seen[0].0, "/kk/api/v1/summary");
    assert_eq!(seen[1].0, "/kk/api/v1/summary");
    assert_eq!(seen[2].0, "/api/v1/summary");
    assert!(seen.iter().all(|(_, q)| q == "hours=24"), "{seen:?}");
}

// ---------------------------------------------------------------------------
// The token stays secret

#[derive(Clone)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn the_token_is_in_no_error_snapshot_debug_output_or_log() {
    let buffer = LogBuffer(Arc::new(Mutex::new(Vec::new())));
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut seen = String::new();
    for (status, body) in [
        (200, summary(totals(1, 1, 2), vec![])),
        (200, "garbage".to_string()),
        (401, format!("echo {TOKEN}")),
        (403, format!("echo Bearer {TOKEN}")),
        (404, format!("echo {TOKEN}")),
        (500, format!("echo {TOKEN}")),
    ] {
        let (url, _) = serve(status, body).await;
        let context = ctx(&url, Some(TOKEN));
        seen += &format!("{context:?}");
        match kk().fetch(&context).await {
            Ok(snapshot) => seen += &format!("{snapshot:?}"),
            Err(err) => {
                seen += &format!("{err} {err:?}");
                seen += &format!("{:?}", Snapshot::failed(&err));
            }
        }
    }
    // A refused connection and a bad token header, too.
    let err = fetch("http://127.0.0.1:1", Some(TOKEN)).await.unwrap_err();
    seen += &format!("{err} {err:?}");
    let err = fetch("http://127.0.0.1:1", Some("bad\ntoken-x"))
        .await
        .unwrap_err();
    seen += &format!("{err} {err:?}");

    let logged = String::from_utf8_lossy(&buffer.0.lock().unwrap()).into_owned();
    // The capture does see what the HTTP library logs.
    assert!(logged.contains("hyper_util"), "nothing was captured");
    for (what, text) in [("results", &seen), ("log", &logged)] {
        assert!(!text.contains(TOKEN), "token in {what}");
        assert!(!text.contains("token-x"), "bad token in {what}");
        assert!(!text.contains("Bearer"), "Bearer in {what}");
    }
    // The library's own debug lines name the address they connect to; the
    // kind's errors never do.
    assert!(!seen.contains("127.0.0.1:"), "address in an error");
}

// ---------------------------------------------------------------------------
// The kind itself

#[test]
fn the_kind_is_registered_with_its_form_metrics_and_intervals() {
    let kind = kk();
    assert_eq!(kind.kind(), "kk");
    assert_eq!(kind.label_key(), "kind.kk");
    assert_eq!(kind.default_interval().as_secs(), 60);
    assert_eq!(kind.min_interval().as_secs(), 30);
    assert!(kind.url_required());

    // The address is the service's own; the only field is the read token.
    let fields = kind.fields();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].key, "secret");
    assert!(fields[0].secret);
    assert!(fields[0].required);

    let ids: Vec<&str> = kind.metrics().iter().map(|m| m.id).collect();
    assert_eq!(
        ids,
        [
            "new_issues",
            "regressed_issues",
            "unresolved_issues",
            "crash_free",
            "visitors_today",
            "app_users_today",
            "newest_issue"
        ]
    );
    let format_of = |id: &str| kind.metrics().iter().find(|m| m.id == id).unwrap().format;
    assert_eq!(format_of("crash_free"), integrations::Format::Percent);
    assert_eq!(format_of("newest_issue"), integrations::Format::Text);
    assert_eq!(format_of("new_issues"), integrations::Format::Number);
}

/// The value of `section.key` in a locale file (a flat reading: `[section]`
/// lines and `key = "text"` lines).
fn locale_text(file: &str, dotted: &str) -> Option<String> {
    let (want_section, want_key) = dotted.split_once('.').unwrap();
    let mut section = String::new();
    for line in file.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_string();
        } else if section == want_section
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == want_key
        {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

#[test]
fn every_text_the_kind_uses_exists_in_english_and_norwegian() {
    let en = include_str!("../locales/en.toml");
    let nb = include_str!("../locales/nb.toml");
    let kind = kk();
    let mut keys: Vec<String> = vec![kind.label_key().to_string()];
    for f in kind.fields() {
        keys.push(f.label_key.to_string());
        if !f.help_key.is_empty() {
            keys.push(f.help_key.to_string());
        }
    }
    keys.extend(kind.metrics().iter().map(|m| m.label_key.to_string()));
    keys.extend(
        [FetchError::Auth, FetchError::KkTooOld]
            .iter()
            .map(|e| e.text_key().unwrap().to_string()),
    );
    keys.extend(["need.kk_regressed".to_string(), "need.kk_new".to_string()]);

    for key in &keys {
        for (lang, file) in [("en", en), ("nb", nb)] {
            let text = locale_text(file, key).unwrap_or_default();
            assert!(!text.is_empty(), "{key} is missing in {lang}");
        }
    }
    // The needs show the count and not the name (the chip has the name).
    for key in ["need.kk_regressed", "need.kk_new"] {
        for file in [en, nb] {
            let text = locale_text(file, key).unwrap();
            assert!(
                text.contains("{count}") && !text.contains("{name}"),
                "{key}"
            );
        }
    }
    assert!(
        locale_text(en, "field.kk_secret_help")
            .unwrap()
            .contains("API tokens")
    );
    assert!(
        locale_text(nb, "field.kk_secret_help")
            .unwrap()
            .contains("API-tokens")
    );
    assert!(
        locale_text(nb, "field.kk_secret_help")
            .unwrap()
            .contains("«Lese»")
    );
    // The version that ships the endpoint is in the text.
    assert!(
        locale_text(en, "fetch_err.kk_too_old")
            .unwrap()
            .contains("0.10")
    );
    assert!(
        locale_text(nb, "fetch_err.kk_too_old")
            .unwrap()
            .contains("0.10")
    );
    assert_eq!(
        locale_text(en, "fetch_err.auth").as_deref(),
        Some("The key was refused")
    );
    assert_eq!(
        locale_text(nb, "fetch_err.auth").as_deref(),
        Some("Nøkkelen ble avvist")
    );
}
