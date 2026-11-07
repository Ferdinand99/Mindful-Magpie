//! The `github` kind against a stub of GitHub's GraphQL and REST APIs: the
//! numbers, the needs, the owners filter, the rate limit, a token that lacks a
//! permission, a refused token, odd answers, and that the token stays out of
//! every error, reading and log.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use jiff::{SignedDuration, Timestamp};
use mindful_magpie::integrations::github::GithubKind;
use mindful_magpie::integrations::{
    self, FetchCtx, FetchError, Format, Integration, Level, Snapshot, Status,
};
use serde_json::{Value as Json, json};

const TOKEN: &str = "github_pat_11S3CR3T-Tok3n-9f8e7d6c5b4a";

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
// A stub of GitHub

fn now_secs() -> i64 {
    Timestamp::now().as_second()
}

/// One answer of the stub.
#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    /// An answer with plenty of the request limit left.
    fn new(status: u16, body: impl Into<String>) -> Reply {
        Reply {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
        .limit(4990, now_secs() + 3600)
    }

    fn ok(body: &Json) -> Reply {
        Reply::new(200, body.to_string())
    }

    /// Sets what the answer says about the request limit.
    fn limit(self, remaining: u64, reset: i64) -> Reply {
        self.header("x-ratelimit-remaining", &remaining.to_string())
            .header("x-ratelimit-reset", &reset.to_string())
    }

    fn header(mut self, name: &str, value: &str) -> Reply {
        self.headers.retain(|(n, _)| n != name);
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// An answer with no word about the limit.
    fn bare(status: u16, body: impl Into<String>) -> Reply {
        Reply {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }
}

/// One request the stub saw.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    query: String,
    auth: Option<String>,
    body: Json,
}

type Script = Arc<Mutex<VecDeque<Reply>>>;

#[derive(Clone, Default)]
struct Stub {
    /// The answers to `{ viewer { login } }`; the last one repeats.
    login: Script,
    /// The answers to the numbers query; the last one repeats.
    main: Script,
    /// Path to answer, for the REST calls.
    rest: Arc<Mutex<HashMap<String, Reply>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn take(script: &Script) -> Option<Reply> {
    let mut script = script.lock().unwrap();
    if script.len() > 1 {
        script.pop_front()
    } else {
        script.front().cloned()
    }
}

impl Stub {
    fn login_is(&self, login: &str) {
        *self.login.lock().unwrap() = VecDeque::from([Reply::ok(
            &json!({ "data": { "viewer": { "login": login } } }),
        )]);
    }

    fn script(&self, replies: Vec<Reply>) {
        *self.main.lock().unwrap() = replies.into();
    }

    fn rest(&self, path: &str, reply: Reply) {
        self.rest.lock().unwrap().insert(path.to_string(), reply);
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// The requests of the numbers query.
    fn mains(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.path == "/graphql" && query_of(s).contains("query Magpie"))
            .collect()
    }

    fn logins(&self) -> usize {
        self.seen()
            .iter()
            .filter(|s| s.path == "/graphql" && !query_of(s).contains("query Magpie"))
            .count()
    }

    fn rests(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.path != "/graphql")
            .collect()
    }
}

fn query_of(seen: &Seen) -> String {
    seen.body["query"].as_str().unwrap_or_default().to_string()
}

async fn handle(State(stub): State<Stub>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    let query = request.uri().query().unwrap_or_default().to_string();
    let auth = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
    let body: Json = serde_json::from_slice(&bytes).unwrap_or(Json::Null);
    stub.seen.lock().unwrap().push(Seen {
        path: path.clone(),
        query,
        auth,
        body: body.clone(),
    });
    let reply = if path == "/graphql" {
        let text = body["query"].as_str().unwrap_or_default();
        if text.contains("query Magpie") {
            take(&stub.main)
        } else {
            take(&stub.login)
        }
    } else {
        stub.rest.lock().unwrap().get(&path).cloned()
    };
    match reply {
        Some(reply) => {
            let mut response = (
                StatusCode::from_u16(reply.status).unwrap(),
                [(header::CONTENT_TYPE, "application/json")],
                reply.body,
            )
                .into_response();
            for (name, value) in reply.headers {
                response.headers_mut().insert(
                    header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                    header::HeaderValue::from_str(&value).unwrap(),
                );
            }
            response
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Starts the stub; the address without a trailing slash.
async fn serve() -> (String, Stub) {
    let stub = Stub::default();
    stub.login_is("octocat");
    let app = Router::new().fallback(handle).with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, stub)
}

// ---------------------------------------------------------------------------
// Fixtures in GitHub's shapes

/// One repository of an answer.
#[derive(Clone)]
struct Repo {
    name: String,
    branch: Option<&'static str>,
    rollup: Option<&'static str>,
    alerts: Option<u64>,
    /// `(draft, hours ago)` of each release; `None` for no `releases` field.
    releases: Option<Vec<(bool, i64)>>,
}

fn repo(name: &str) -> Repo {
    Repo {
        name: name.to_string(),
        branch: Some("main"),
        rollup: Some("SUCCESS"),
        alerts: Some(0),
        releases: Some(Vec::new()),
    }
}

fn ago(hours: i64) -> String {
    Timestamp::now()
        .checked_sub(SignedDuration::from_hours(hours))
        .unwrap()
        .to_string()
}

fn node(r: &Repo) -> Json {
    json!({
        "nameWithOwner": r.name,
        "defaultBranchRef": r.branch.map(|b| json!({
            "name": b,
            "target": { "statusCheckRollup": r.rollup.map(|s| json!({ "state": s })) },
        })),
        "vulnerabilityAlerts": r.alerts.map(|n| json!({ "totalCount": n })),
        "releases": r.releases.as_ref().map(|list| json!({
            "nodes": list.iter().map(|(draft, hours)| json!({
                "publishedAt": ago(*hours),
                "isDraft": draft,
            })).collect::<Vec<_>>(),
        })),
    })
}

fn repositories(repos: &[Repo]) -> Json {
    json!({ "repositories": { "nodes": repos.iter().map(node).collect::<Vec<_>>() } })
}

fn counts(data: &mut Json, reviews: Option<u64>, authored: Option<u64>) {
    data["rateLimit"] = json!({ "cost": 1, "remaining": 4990, "resetAt": ago(-1) });
    if let Some(n) = reviews {
        // Pull request titles and repositories are in the answer, as in the
        // real one for a wide query: the kind must not keep them.
        data["reviews"] = json!({ "issueCount": n, "nodes": [{ "title": "Secret PR title" }] });
    }
    if let Some(n) = authored {
        data["authored"] = json!({ "issueCount": n });
    }
}

/// The answer to the numbers query for an account's own repositories.
fn answer(repos: &[Repo], reviews: Option<u64>, authored: Option<u64>) -> Json {
    let mut data = json!({ "viewer": repositories(repos) });
    counts(&mut data, reviews, authored);
    json!({ "data": data })
}

fn ctx(id: Option<i64>, owners: Option<&str>, secret: Option<&str>) -> FetchCtx {
    let mut settings = json!({});
    if let Some(id) = id {
        settings["service_id"] = json!(id);
    }
    if let Some(owners) = owners {
        settings["owners"] = json!(owners);
    }
    FetchCtx {
        http: integrations::build_client(Duration::from_secs(5)),
        settings,
        secret: secret.map(str::to_string),
    }
}

/// A number no other test's service has (what the kind keeps in memory is
/// per service).
fn next_id() -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(9_000_000);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

async fn fetch_with(base: &str, ctx: &FetchCtx) -> Result<Snapshot, FetchError> {
    GithubKind::with_base(base).fetch(ctx).await
}

/// A fetch of a stored service that is expected to give a reading.
async fn fetch(base: &str, id: i64) -> Snapshot {
    fetch_with(base, &ctx(Some(id), None, Some(TOKEN)))
        .await
        .expect("a reading")
}

async fn fetch_err(base: &str, id: i64) -> FetchError {
    fetch_with(base, &ctx(Some(id), None, Some(TOKEN)))
        .await
        .expect_err("no reading")
}

fn num(snapshot: &Snapshot, id: &str) -> Option<f64> {
    snapshot.values.get(id).and_then(|v| v.num)
}

fn metric_ids(snapshot: &Snapshot) -> Vec<&str> {
    snapshot.values.keys().map(String::as_str).collect()
}

// ---------------------------------------------------------------------------
// The kind

#[test]
fn the_kind_describes_itself() {
    let kind = integrations::find("github").expect("the registry lists github");
    assert_eq!(kind.kind(), "github");
    assert_eq!(kind.label_key(), "kind.github");
    assert_eq!(kind.default_interval(), Duration::from_secs(300));
    assert_eq!(kind.min_interval(), Duration::from_secs(120));
    // A personal kind with a fixed address: nothing to enter.
    assert!(kind.personal());
    assert!(!kind.url_required());
    let metrics: Vec<_> = kind.metrics().iter().map(|m| (m.id, m.format)).collect();
    assert_eq!(
        metrics,
        [
            ("review_requests", Format::Number),
            ("ci_red", Format::Number),
            ("dependabot", Format::Number),
            ("releases_24h", Format::Number),
            ("open_prs", Format::Number),
        ]
    );
    let fields = kind.fields();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].key, "owners");
    assert!(!fields[0].secret && !fields[0].required);
    assert_eq!(fields[1].key, "secret");
    assert!(fields[1].secret && fields[1].required);
    assert_eq!(fields[1].placeholder, "github_pat_…");
    // No other kind is personal.
    for other in integrations::registry() {
        assert_eq!(
            other.personal(),
            other.kind() == "github",
            "{}",
            other.kind()
        );
    }
}

#[test]
fn the_owners_setting_is_checked() {
    let kind = integrations::find("github").unwrap();
    let check = |settings: Json| kind.check_settings(settings.as_object().unwrap());
    assert!(check(json!({})).is_ok());
    assert!(check(json!({ "owners": "" })).is_ok());
    assert!(check(json!({ "owners": "me, my-org" })).is_ok());
    for bad in ["a/b", "user:evil", "-x", "a b/c", "a,b,c,d,e,f"] {
        let err = check(json!({ "owners": bad })).unwrap_err();
        assert_eq!(err.field, "owners", "{bad}");
        assert_eq!(err.message_key, "err.github_owners", "{bad}");
    }
    assert!(check(json!({ "owners": 5 })).is_err());
}

#[tokio::test]
async fn bad_settings_are_a_config_error_and_nothing_is_sent() {
    let (base, stub) = serve().await;
    let kind = GithubKind::with_base(&base);
    for owners in ["a/b", "x y/z", "a,b,c,d,e,f"] {
        let err = kind
            .fetch(&ctx(None, Some(owners), Some(TOKEN)))
            .await
            .unwrap_err();
        assert_eq!(err, FetchError::Config, "{owners}");
    }
    for secret in [None, Some(""), Some("a\r\nb")] {
        let err = kind.fetch(&ctx(None, None, secret)).await.unwrap_err();
        assert_eq!(err, FetchError::Config, "{secret:?}");
    }
    assert!(stub.seen().is_empty());
}

// ---------------------------------------------------------------------------
// The numbers

#[tokio::test]
async fn one_query_gives_every_number_and_the_needs() {
    let (base, stub) = serve().await;
    // Red by the answer, green, and one the answer says nothing about whose
    // newest workflow run failed (the REST call).
    let mut red = repo("me/api");
    red.rollup = Some("FAILURE");
    red.alerts = Some(1);
    red.releases = Some(vec![(false, 2)]);
    let mut green = repo("me/web");
    green.alerts = Some(2);
    // A draft, and one that is two days old: neither counts.
    green.releases = Some(vec![(true, 1), (false, 50)]);
    let mut unknown = repo("me/lib");
    unknown.rollup = None;
    stub.script(vec![Reply::ok(&answer(
        &[red, green, unknown],
        Some(3),
        Some(2),
    ))]);
    stub.rest(
        "/repos/me/lib/actions/runs",
        Reply::ok(&json!({ "workflow_runs": [
            { "id": 9, "workflow_id": 1, "conclusion": "failure" },
            { "id": 4, "workflow_id": 1, "conclusion": "success" },
        ] })),
    );

    let snapshot = fetch(&base, next_id()).await;
    assert_eq!(snapshot.error, None);
    assert_eq!(num(&snapshot, "review_requests"), Some(3.0));
    assert_eq!(num(&snapshot, "open_prs"), Some(2.0));
    assert_eq!(num(&snapshot, "ci_red"), Some(2.0));
    assert_eq!(num(&snapshot, "dependabot"), Some(3.0));
    assert_eq!(num(&snapshot, "releases_24h"), Some(1.0));
    assert_eq!(snapshot.status, Status::Bad);

    // The needs hold counts only: the key, the level, the label's key.
    assert_eq!(snapshot.needs.len(), 2);
    let ci = &snapshot.needs[0];
    assert_eq!(ci.key, "gh:ci_red");
    assert_eq!(ci.level, Level::Bad);
    assert_eq!(ci.label_key, "need.gh_ci_red");
    assert_eq!(ci.params["count"], "2");
    assert_eq!(ci.score, 2.0);
    assert_eq!(ci.params.len(), 1);
    let review = &snapshot.needs[1];
    assert_eq!(review.key, "gh:review");
    assert_eq!(review.level, Level::Warn);
    assert_eq!(review.label_key, "need.gh_review");
    assert_eq!(review.params["count"], "3");
    assert_eq!(review.score, 3.0);

    // Nothing GitHub called something is kept.
    let stored = serde_json::to_string(&snapshot).unwrap();
    for text in ["Secret PR title", "me/api", "me/web", "me/lib", "octocat"] {
        assert!(!stored.contains(text), "{text}: {stored}");
    }

    // One query for the numbers and one call for the repository that needed
    // it, the token on both.
    assert_eq!(stub.mains().len(), 1);
    let rests = stub.rests();
    assert_eq!(rests.len(), 1);
    assert!(rests[0].query.contains("branch=main"), "{}", rests[0].query);
    assert!(rests[0].query.contains("status=completed"));
    assert!(rests[0].query.contains("exclude_pull_requests=true"));
    for seen in stub.seen() {
        assert_eq!(
            seen.auth.as_deref(),
            Some(format!("Bearer {TOKEN}").as_str())
        );
    }
}

#[tokio::test]
async fn a_quiet_account_is_ok_with_zeros_and_no_needs() {
    let (base, stub) = serve().await;
    stub.script(vec![Reply::ok(&answer(&[repo("me/a")], Some(0), Some(0)))]);
    let snapshot = fetch(&base, next_id()).await;
    for id in [
        "review_requests",
        "open_prs",
        "ci_red",
        "dependabot",
        "releases_24h",
    ] {
        assert_eq!(num(&snapshot, id), Some(0.0), "{id}");
    }
    assert_eq!(snapshot.status, Status::Ok);
    assert!(snapshot.needs.is_empty());
    assert_eq!(snapshot.error, None);
    assert!(
        stub.rests().is_empty(),
        "a green repository needs no REST call"
    );
}

#[tokio::test]
async fn reviews_waiting_make_a_warning_and_red_ci_makes_it_bad() {
    let (base, stub) = serve().await;
    stub.script(vec![Reply::ok(&answer(&[repo("me/a")], Some(4), Some(1)))]);
    let snapshot = fetch(&base, next_id()).await;
    assert_eq!(snapshot.status, Status::Warn);
    assert_eq!(snapshot.needs.len(), 1);
    assert_eq!(snapshot.needs[0].key, "gh:review");
    assert_eq!(snapshot.needs[0].score, 4.0);
}

#[tokio::test]
async fn no_repositories_is_zero_not_missing() {
    let (base, stub) = serve().await;
    stub.script(vec![Reply::ok(&answer(&[], Some(0), Some(0)))]);
    let snapshot = fetch(&base, next_id()).await;
    assert_eq!(num(&snapshot, "ci_red"), Some(0.0));
    assert_eq!(num(&snapshot, "dependabot"), Some(0.0));
    assert_eq!(num(&snapshot, "releases_24h"), Some(0.0));
}

#[tokio::test]
async fn the_login_is_asked_once_and_used_in_the_searches() {
    let (base, stub) = serve().await;
    stub.script(vec![Reply::ok(&answer(&[repo("me/a")], Some(1), Some(1)))]);
    let id = next_id();
    fetch(&base, id).await;
    fetch(&base, id).await;
    assert_eq!(stub.logins(), 1, "remembered for the service");
    let mains = stub.mains();
    assert_eq!(mains.len(), 2);
    let vars = &mains[0].body["variables"];
    assert_eq!(
        vars["reviewQ"],
        "is:pr is:open review-requested:octocat archived:false"
    );
    assert_eq!(
        vars["authoredQ"],
        "is:pr is:open author:octocat archived:false"
    );

    // A test of a form has no service to remember it for.
    let before = stub.logins();
    for _ in 0..2 {
        fetch_with(&base, &ctx(None, None, Some(TOKEN)))
            .await
            .unwrap();
    }
    assert_eq!(stub.logins(), before + 2);

    // Another token for the same service is another account.
    stub.login_is("someone-else");
    fetch_with(&base, &ctx(Some(id), None, Some("github_pat_other")))
        .await
        .unwrap();
    let last = stub.mains().pop().unwrap();
    assert_eq!(
        last.body["variables"]["reviewQ"],
        "is:pr is:open review-requested:someone-else archived:false"
    );
}

#[tokio::test]
async fn the_owners_filter_goes_in_as_variables() {
    let (base, stub) = serve().await;
    let mut data = json!({
        "o0": repositories(&[repo("acme/one")]),
        "o1": repositories(&[repo("me/two"), repo("acme/one")]),
    });
    counts(&mut data, Some(2), Some(1));
    stub.script(vec![Reply::ok(&json!({ "data": data }))]);
    let snapshot = fetch_with(&base, &ctx(Some(next_id()), Some("acme, me"), Some(TOKEN)))
        .await
        .unwrap();
    assert_eq!(snapshot.status, Status::Warn);
    let seen = stub.mains().pop().unwrap();
    let text = query_of(&seen);
    assert!(text.contains("repositoryOwner(login: $o0)"), "{text}");
    // Only repositories the account owns, as in the token account's own branch.
    assert_eq!(
        text.matches("ownerAffiliations: [OWNER]").count(),
        2,
        "{text}"
    );
    assert!(!text.contains("viewer {"), "no own repositories: {text}");
    assert!(!text.contains("acme"), "names are variables only: {text}");
    assert_eq!(seen.body["variables"]["o0"], "acme");
    assert_eq!(seen.body["variables"]["o1"], "me");
    assert_eq!(
        seen.body["variables"]["reviewQ"],
        "is:pr is:open review-requested:octocat archived:false user:acme user:me"
    );
    // The same repository under two owners is counted once.
    assert_eq!(num(&snapshot, "dependabot"), Some(0.0));
}

#[tokio::test]
async fn the_workflow_runs_decide_for_a_repository_the_answer_is_silent_about() {
    let (base, stub) = serve().await;
    let (mut green, mut cancelled) = (repo("me/green"), repo("me/cancelled"));
    green.rollup = None;
    cancelled.rollup = None;
    stub.script(vec![Reply::ok(&answer(
        &[green, cancelled],
        Some(0),
        Some(0),
    ))]);
    // The newest run passed after an older failure.
    stub.rest(
        "/repos/me/green/actions/runs",
        Reply::ok(&json!({ "workflow_runs": [
            { "id": 8, "workflow_id": 1, "conclusion": "success" },
            { "id": 3, "workflow_id": 1, "conclusion": "failure" },
        ] })),
    );
    // A newer cancelled run does not hide the failure before it.
    stub.rest(
        "/repos/me/cancelled/actions/runs",
        Reply::ok(&json!({ "workflow_runs": [
            { "id": 8, "workflow_id": 1, "conclusion": "cancelled" },
            { "id": 3, "workflow_id": 1, "conclusion": "timed_out" },
        ] })),
    );
    let snapshot = fetch(&base, next_id()).await;
    assert_eq!(num(&snapshot, "ci_red"), Some(1.0));
}

#[tokio::test]
async fn at_most_ten_repositories_are_asked_for_their_runs() {
    let (base, stub) = serve().await;
    let repos: Vec<Repo> = (0..14)
        .map(|i| {
            let mut r = repo(&format!("me/r{i}"));
            r.rollup = None;
            r
        })
        .collect();
    stub.script(vec![Reply::ok(&answer(&repos, Some(0), Some(0)))]);
    for i in 0..14 {
        stub.rest(
            &format!("/repos/me/r{i}/actions/runs"),
            Reply::ok(&json!({ "workflow_runs": [] })),
        );
    }
    let snapshot = fetch(&base, next_id()).await;
    assert_eq!(stub.rests().len(), 10);
    assert_eq!(num(&snapshot, "ci_red"), Some(0.0));
}

// ---------------------------------------------------------------------------
// The rate limit

#[tokio::test]
async fn a_used_up_limit_makes_the_service_wait_for_the_reset() {
    let (base, stub) = serve().await;
    // This answer is a reading, and it says nothing is left until the reset.
    stub.script(vec![
        Reply::ok(&answer(&[repo("me/a")], Some(1), Some(1))).limit(0, now_secs() + 3600),
    ]);
    let id = next_id();
    let first = fetch(&base, id).await;
    assert_eq!(num(&first, "review_requests"), Some(1.0));
    let sent = stub.seen().len();

    // Until the reset no request goes out, and the reason is the limit.
    for _ in 0..3 {
        assert_eq!(fetch_err(&base, id).await, FetchError::RateLimited);
    }
    assert_eq!(stub.seen().len(), sent, "nothing was sent while waiting");

    // Another service is not held back.
    fetch(&base, next_id()).await;
    assert!(stub.seen().len() > sent);

    // A new token for the same service starts afresh.
    let before = stub.seen().len();
    fetch_with(&base, &ctx(Some(id), None, Some("github_pat_replaced")))
        .await
        .unwrap();
    assert!(stub.seen().len() > before);
}

#[tokio::test]
async fn a_refusal_for_the_limit_waits_and_the_wait_ends_at_the_reset() {
    let (base, stub) = serve().await;
    let reset = now_secs() + 2;
    stub.script(vec![
        Reply::new(403, "{\"message\": \"API rate limit exceeded\"}").limit(0, reset),
        Reply::ok(&answer(&[repo("me/a")], Some(1), Some(0))),
    ]);
    let id = next_id();
    assert_eq!(fetch_err(&base, id).await, FetchError::RateLimited);
    let sent = stub.seen().len();
    assert_eq!(fetch_err(&base, id).await, FetchError::RateLimited);
    assert_eq!(stub.seen().len(), sent);

    tokio::time::sleep(Duration::from_millis(3200)).await;
    let snapshot = fetch(&base, id).await;
    assert_eq!(num(&snapshot, "review_requests"), Some(1.0));
    assert!(stub.seen().len() > sent, "asked again after the reset");
}

#[tokio::test]
async fn retry_after_and_the_graphql_rate_limit_error_also_make_it_wait() {
    let (base, stub) = serve().await;
    // A secondary limit: 429 with Retry-After.
    stub.script(vec![
        Reply::bare(429, "slow down").header("retry-after", "120"),
    ]);
    let id = next_id();
    assert_eq!(fetch_err(&base, id).await, FetchError::RateLimited);
    let sent = stub.seen().len();
    assert_eq!(fetch_err(&base, id).await, FetchError::RateLimited);
    assert_eq!(stub.seen().len(), sent);

    // GraphQL says it with a 200 and an error of type RATE_LIMITED.
    let (base, stub) = serve().await;
    stub.script(vec![
        Reply::ok(&json!({ "data": null, "errors": [{ "type": "RATE_LIMITED", "message": "x" }] }))
            .limit(0, now_secs() + 600),
    ]);
    let id = next_id();
    assert_eq!(fetch_err(&base, id).await, FetchError::RateLimited);
    let sent = stub.seen().len();
    assert_eq!(fetch_err(&base, id).await, FetchError::RateLimited);
    assert_eq!(stub.seen().len(), sent);
}

#[tokio::test]
async fn a_secondary_limit_in_the_body_waits_a_minute_and_is_not_a_refused_key() {
    for status in [403, 429] {
        let (base, stub) = serve().await;
        stub.script(vec![Reply::bare(
            status,
            "{\"message\": \"You have exceeded a secondary rate limit. Please wait.\"}",
        )]);
        let id = next_id();
        assert_eq!(
            fetch_err(&base, id).await,
            FetchError::RateLimited,
            "{status}"
        );
        let sent = stub.seen().len();
        assert_eq!(
            fetch_err(&base, id).await,
            FetchError::RateLimited,
            "{status}"
        );
        assert_eq!(stub.seen().len(), sent, "{status}: it waits");
    }
    // The same words on the REST side make the service wait too.
    let (base, stub) = serve().await;
    let mut plain = repo("me/a");
    plain.rollup = None;
    stub.script(vec![Reply::ok(&answer(&[plain], Some(1), Some(0)))]);
    stub.rest(
        "/repos/me/a/actions/runs",
        Reply::bare(403, "You have exceeded a secondary rate limit"),
    );
    let id = next_id();
    let snapshot = fetch(&base, id).await;
    assert_eq!(num(&snapshot, "ci_red"), None);
    assert_eq!(fetch_err(&base, id).await, FetchError::RateLimited);
}

#[tokio::test]
async fn a_part_that_failed_for_another_reason_is_left_out_of_this_fetch_only() {
    let (base, stub) = serve().await;
    let mut no_alerts = repo("me/a");
    no_alerts.alerts = None;
    let mut first = answer(&[no_alerts.clone()], Some(1), Some(0));
    first["errors"] = json!([{
        "type": "SERVICE_UNAVAILABLE",
        "path": ["viewer", "repositories", "nodes", 0, "vulnerabilityAlerts"],
    }]);
    // The first answer fails the alerts, the one after has them again.
    let mut with_alerts = repo("me/a");
    with_alerts.alerts = Some(2);
    stub.script(vec![
        Reply::ok(&first),
        Reply::ok(&answer(&[no_alerts], Some(1), Some(0))),
        Reply::ok(&answer(&[with_alerts], Some(1), Some(0))),
    ]);
    let id = next_id();
    let snapshot = fetch(&base, id).await;
    assert_eq!(snapshot.error, None);
    assert_eq!(num(&snapshot, "dependabot"), None);
    let mains = stub.mains();
    assert_eq!(mains.len(), 2);
    assert!(!query_of(&mains[1]).contains("vulnerabilityAlerts"));
    // Not remembered: the next fetch asks for them again.
    let snapshot = fetch(&base, id).await;
    let mains = stub.mains();
    assert_eq!(mains.len(), 3);
    assert!(query_of(&mains[2]).contains("vulnerabilityAlerts"));
    assert_eq!(num(&snapshot, "dependabot"), Some(2.0));
}

#[tokio::test]
async fn a_refusal_that_is_not_the_limit_is_a_refused_key_and_nothing_waits() {
    let (base, stub) = serve().await;
    stub.script(vec![Reply::bare(
        403,
        "{\"message\": \"Resource not accessible\"}",
    )]);
    let id = next_id();
    assert_eq!(fetch_err(&base, id).await, FetchError::Auth);
    let sent = stub.seen().len();
    assert_eq!(fetch_err(&base, id).await, FetchError::Auth);
    assert!(stub.seen().len() > sent, "it tries again");
}

#[tokio::test]
async fn few_requests_left_skips_the_optional_calls() {
    let (base, stub) = serve().await;
    let mut unknown = repo("me/lib");
    unknown.rollup = None;
    stub.script(vec![
        Reply::ok(&answer(&[unknown], Some(1), Some(0))).limit(10, now_secs() + 3600),
    ]);
    let snapshot = fetch(&base, next_id()).await;
    assert!(stub.rests().is_empty());
    // Without a word about CI the number is left out, not shown as 0.
    assert_eq!(num(&snapshot, "ci_red"), None);
    assert_eq!(num(&snapshot, "review_requests"), Some(1.0));
}

// ---------------------------------------------------------------------------
// A missing permission gives fewer numbers

fn forbidden(path: Json) -> Json {
    json!({ "type": "FORBIDDEN", "path": path, "message": "Resource not accessible by personal access token" })
}

#[tokio::test]
async fn a_token_without_dependabot_access_gives_the_other_numbers() {
    let (base, stub) = serve().await;
    let mut no_alerts = repo("me/a");
    no_alerts.alerts = None;
    no_alerts.releases = Some(vec![(false, 1)]);
    let mut first = answer(&[no_alerts.clone()], Some(1), Some(2));
    first["errors"] = json!([forbidden(json!([
        "viewer",
        "repositories",
        "nodes",
        0,
        "vulnerabilityAlerts"
    ]))]);
    stub.script(vec![
        Reply::ok(&first),
        Reply::ok(&answer(&[no_alerts], Some(1), Some(2))),
    ]);
    let id = next_id();
    let snapshot = fetch(&base, id).await;
    assert_eq!(snapshot.error, None, "fewer numbers, not an error");
    assert_eq!(num(&snapshot, "dependabot"), None);
    assert_eq!(num(&snapshot, "review_requests"), Some(1.0));
    assert_eq!(num(&snapshot, "open_prs"), Some(2.0));
    assert_eq!(num(&snapshot, "ci_red"), Some(0.0));
    assert_eq!(num(&snapshot, "releases_24h"), Some(1.0));
    // The query was sent again without the part that was refused…
    let mains = stub.mains();
    assert_eq!(mains.len(), 2);
    assert!(query_of(&mains[0]).contains("vulnerabilityAlerts"));
    assert!(!query_of(&mains[1]).contains("vulnerabilityAlerts"));
    // …and the next fetch leaves it out at once.
    fetch(&base, id).await;
    let mains = stub.mains();
    assert_eq!(mains.len(), 3);
    assert!(!query_of(&mains[2]).contains("vulnerabilityAlerts"));
}

#[tokio::test]
async fn a_token_that_cannot_search_pull_requests_gives_the_repository_numbers() {
    let (base, stub) = serve().await;
    let mut data = json!({ "viewer": repositories(&[repo("me/a")]) });
    counts(&mut data, None, None);
    stub.script(vec![
        // `search` can't be null, so the refusal takes the whole answer.
        Reply::ok(&json!({ "data": null, "errors": [forbidden(json!(["reviews"]))] })),
        Reply::ok(&json!({ "data": data })),
    ]);
    let snapshot = fetch(&base, next_id()).await;
    assert_eq!(snapshot.error, None);
    assert_eq!(num(&snapshot, "review_requests"), None);
    assert_eq!(num(&snapshot, "open_prs"), None);
    assert_eq!(num(&snapshot, "ci_red"), Some(0.0));
    let mains = stub.mains();
    assert_eq!(mains.len(), 2);
    assert!(!query_of(&mains[1]).contains("search("));
}

#[tokio::test]
async fn a_token_that_cannot_read_check_status_falls_back_to_the_workflow_runs() {
    let (base, stub) = serve().await;
    let mut plain = repo("me/a");
    plain.rollup = None;
    let mut first = answer(&[plain.clone()], Some(0), Some(0));
    first["errors"] = json!([forbidden(json!([
        "viewer",
        "repositories",
        "nodes",
        0,
        "defaultBranchRef",
        "target",
        "statusCheckRollup"
    ]))]);
    stub.script(vec![
        Reply::ok(&first),
        Reply::ok(&answer(&[plain], Some(0), Some(0))),
    ]);
    stub.rest(
        "/repos/me/a/actions/runs",
        Reply::ok(
            &json!({ "workflow_runs": [{ "id": 2, "workflow_id": 7, "conclusion": "failure" }] }),
        ),
    );
    let snapshot = fetch(&base, next_id()).await;
    assert_eq!(num(&snapshot, "ci_red"), Some(1.0));
    let mains = stub.mains();
    assert!(!query_of(&mains[1]).contains("statusCheckRollup"));
}

#[tokio::test]
async fn a_token_without_actions_access_leaves_the_ci_number_out() {
    let (base, stub) = serve().await;
    let mut plain = repo("me/a");
    plain.rollup = None;
    stub.script(vec![Reply::ok(&answer(&[plain], Some(2), Some(0)))]);
    // Resource not accessible for the Actions API.
    stub.rest(
        "/repos/me/a/actions/runs",
        Reply::bare(
            403,
            "{\"message\": \"Resource not accessible by personal access token\"}",
        ),
    );
    let snapshot = fetch(&base, next_id()).await;
    assert_eq!(snapshot.error, None, "fewer numbers, not an error");
    assert_eq!(num(&snapshot, "ci_red"), None);
    assert_eq!(num(&snapshot, "review_requests"), Some(2.0));
    assert_eq!(num(&snapshot, "dependabot"), Some(0.0));
    assert_eq!(snapshot.status, Status::Warn);
}

#[tokio::test]
async fn odd_values_only_drop_their_own_number() {
    let (base, stub) = serve().await;
    let mut data = json!({ "viewer": repositories(&[repo("me/a")]) });
    counts(&mut data, Some(1), Some(1));
    data["reviews"]["issueCount"] = json!("many");
    data["authored"]["issueCount"] = json!(-4);
    data["viewer"]["repositories"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(Json::Null);
    stub.script(vec![Reply::ok(&json!({ "data": data }))]);
    let snapshot = fetch(&base, next_id()).await;
    assert_eq!(
        metric_ids(&snapshot),
        ["ci_red", "dependabot", "releases_24h"]
    );
}

// ---------------------------------------------------------------------------
// Refused, odd and failing answers

#[tokio::test]
async fn a_refused_token_is_a_refused_key_at_the_login_and_at_the_numbers() {
    let (base, stub) = serve().await;
    stub.login.lock().unwrap().clear();
    stub.login
        .lock()
        .unwrap()
        .push_back(Reply::bare(401, "{\"message\": \"Bad credentials\"}"));
    assert_eq!(fetch_err(&base, next_id()).await, FetchError::Auth);
    assert!(stub.mains().is_empty(), "nothing more is asked");

    // The login is remembered, then the token expires.
    let (base, stub) = serve().await;
    stub.script(vec![
        Reply::ok(&answer(&[repo("me/a")], Some(0), Some(0))),
        Reply::bare(401, "{\"message\": \"Bad credentials\"}"),
    ]);
    let id = next_id();
    fetch(&base, id).await;
    assert_eq!(fetch_err(&base, id).await, FetchError::Auth);
}

#[tokio::test]
async fn failing_malformed_and_oversized_answers_are_fixed_reasons() {
    let (base, stub) = serve().await;
    let cases: Vec<(Reply, FetchError)> = vec![
        (Reply::bare(500, "boom"), FetchError::Status(500)),
        (Reply::bare(502, "{}"), FetchError::Status(502)),
        (Reply::bare(404, "{}"), FetchError::Status(404)),
        (Reply::new(200, "not json at all {"), FetchError::Invalid),
        (Reply::new(200, "[1, 2]"), FetchError::Invalid),
        (Reply::new(200, "{\"data\": 5}"), FetchError::Invalid),
        (Reply::new(200, "{\"data\": null}"), FetchError::Invalid),
        // Nothing in it that could be a number.
        (Reply::new(200, "{\"data\": {}}"), FetchError::Invalid),
        (Reply::new(200, ""), FetchError::Invalid),
        (
            Reply::new(200, "x".repeat(2 * 1024 * 1024)),
            FetchError::TooLarge,
        ),
    ];
    for (reply, want) in cases {
        let shown = format!(
            "{} {}",
            reply.status,
            &reply.body[..reply.body.len().min(20)]
        );
        stub.script(vec![reply]);
        assert_eq!(fetch_err(&base, next_id()).await, want, "{shown}");
    }
    // Nobody answers.
    let err = fetch_with("http://127.0.0.1:1", &ctx(None, None, Some(TOKEN)))
        .await
        .unwrap_err();
    assert_eq!(err, FetchError::Connect);
}

#[tokio::test]
async fn a_redirect_to_another_origin_does_not_carry_the_token() {
    let (other, other_stub) = serve().await;
    let (base, stub) = serve().await;
    stub.login.lock().unwrap().clear();
    stub.login
        .lock()
        .unwrap()
        .push_back(Reply::bare(307, "").header("location", &format!("{other}/graphql")));
    let _ = fetch_with(&base, &ctx(None, None, Some(TOKEN))).await;
    // A request with a key never follows a redirect to another origin: the
    // other server saw nothing at all, so no key reached it.
    let seen = other_stub.seen();
    assert!(seen.is_empty(), "the redirect was followed: {seen:?}");
}

// ---------------------------------------------------------------------------
// The token stays secret

#[tokio::test]
async fn the_token_is_in_no_reading_error_or_log() {
    let _ = log_buffer();
    let (base, stub) = serve().await;
    let mut readings = Vec::new();
    let mut errors = Vec::new();

    let mut red = repo("me/a");
    red.rollup = Some("FAILURE");
    stub.script(vec![Reply::ok(&answer(&[red], Some(2), Some(1)))]);
    readings.push(fetch(&base, next_id()).await);

    // Answers that echo the token back.
    let echo = format!("Bearer {TOKEN} {TOKEN}");
    for reply in [
        Reply::bare(500, echo.clone()),
        Reply::bare(401, echo.clone()),
        Reply::bare(403, echo.clone()),
        Reply::new(200, format!("{{\"data\": \"{TOKEN}\"}}")),
        Reply::new(200, echo.clone()),
        Reply::new(403, echo.clone()).limit(0, now_secs() + 60),
    ] {
        stub.script(vec![reply]);
        errors.push(fetch_err(&base, next_id()).await);
    }
    errors.push(
        fetch_with("http://127.0.0.1:1", &ctx(None, None, Some(TOKEN)))
            .await
            .unwrap_err(),
    );
    for err in &errors {
        assert!(!format!("{err} {err:?}").contains(TOKEN), "{err}");
    }
    for reading in &readings {
        let stored = serde_json::to_string(reading).unwrap();
        assert!(!stored.contains(TOKEN), "{stored}");
        assert!(!stored.contains("Bearer"), "{stored}");
        assert!(!format!("{reading:?}").contains(TOKEN));
    }
    let c = ctx(Some(1), None, Some(TOKEN));
    assert!(!format!("{c:?}").contains(TOKEN));

    let log = logged();
    // The capture does see what the HTTP library logs.
    assert!(
        log.contains("hyper_util") || log.contains("reqwest"),
        "the capture sees the HTTP client's own lines"
    );
    assert!(!log.contains(TOKEN), "the token reached the log");
    assert!(
        !log.contains("Bearer"),
        "an Authorization value reached the log"
    );
}

// ---------------------------------------------------------------------------
// The texts

/// The texts of one `[section]` of a locale file.
fn section(file: &str, wanted: &str) -> HashMap<String, String> {
    let mut current = "";
    let mut texts = HashMap::new();
    for line in file.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name;
        } else if current == wanted
            && let Some((key, value)) = line.split_once(" = ")
        {
            texts.insert(key.to_string(), value.trim_matches('"').to_string());
        }
    }
    texts
}

#[test]
fn every_text_of_the_kind_is_in_both_languages() {
    let kind = integrations::find("github").unwrap();
    for (lang, file) in [
        ("en", include_str!("../locales/en.toml")),
        ("nb", include_str!("../locales/nb.toml")),
    ] {
        let has = |sec: &str, key: &str| {
            section(file, sec)
                .get(key)
                .is_some_and(|text| !text.is_empty())
        };
        assert!(has("kind", "github"), "{lang}");
        assert!(has("nav", "my_integrations"), "{lang}");
        assert!(has("err", "github_owners"), "{lang}");
        assert!(has("fetch_err", "rate_limited"), "{lang}");
        for field in kind.fields() {
            for key in [field.label_key, field.help_key] {
                let key = key.strip_prefix("field.").unwrap();
                assert!(has("field", key), "{lang}: {key}");
            }
        }
        for metric in kind.metrics() {
            let key = metric.label_key.strip_prefix("metric.").unwrap();
            assert!(has("metric", key), "{lang}: {key}");
        }
        for key in [
            "title",
            "sub",
            "new",
            "edit",
            "none_title",
            "none_text",
            "created",
            "saved",
            "deleted",
        ] {
            // `title` is the nav label, the rest are the integration page's.
            if key != "title" {
                assert!(has("integration", key), "{lang}: {key}");
            }
        }

        // The needs: the noun, a colon and the count, and nothing else.
        let needs = section(file, "need");
        let (review, red) = match lang {
            "en" => ("Waiting for you: {count}", "Red CI: {count}"),
            _ => ("Venter på deg: {count}", "Rød CI: {count}"),
        };
        assert_eq!(needs["gh_review"], review, "{lang}");
        assert_eq!(needs["gh_ci_red"], red, "{lang}");

        // The help lists the permissions, and says one token covers one owner.
        let help = &section(file, "field")["github_secret_help"];
        for permission in [
            "Metadata",
            "Pull requests",
            "Commit statuses",
            "Actions",
            "Dependabot alerts",
            "Contents",
        ] {
            assert!(help.contains(permission), "{lang}: {permission}");
        }
        assert!(help.contains("Authorization: Bearer"), "{lang}");
    }
}

#[test]
fn the_rate_limit_has_a_fixed_text_with_a_translation() {
    assert_eq!(FetchError::RateLimited.to_string(), "rate limited");
    assert_eq!(
        integrations::error_text_key("rate limited"),
        Some("fetch_err.rate_limited")
    );
    assert_eq!(
        FetchError::RateLimited.text_key(),
        Some("fetch_err.rate_limited")
    );
}
