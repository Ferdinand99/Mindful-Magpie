//! The `github` kind: a user's own GitHub numbers (reviews waiting for them,
//! their open pull requests, red CI on default branches, open Dependabot
//! alerts, releases of the last day), read with a fine-grained personal access
//! token that has read access only.
//!
//! **Personal.** The address is fixed in the kind (`https://api.github.com`):
//! a service has no address of its own, so there is no address to point at an
//! internal host. The token is the user's own, and so is the service (see
//! [`Integration::personal`]). One fine-grained token covers one owner or
//! organisation, so a user adds a service for each. The optional `owners`
//! setting narrows the repositories to the named users and organisations (the
//! token's own account when empty).
//!
//! **Calls.** One GraphQL query a fetch asks for the repositories (default
//! branch status, open Dependabot alerts, the newest releases) and, with the
//! user's login (asked once, then remembered), the number of pull requests
//! waiting for their review and of their own open ones. A repository whose
//! default branch has no status in the answer gets one REST call (the newest
//! completed workflow runs on that branch), for at most [`CI_FALLBACK_MAX`]
//! repositories a fetch. Repositories past the first 100 of an owner are not
//! counted.
//!
//! **A missing permission gives fewer numbers, not an error.** A part of the
//! query the token may not read (the answer's errors name its path) is left out
//! of the next ones for an hour and the metrics that need it are absent. A
//! refused token (401) is [`FetchError::Auth`].
//!
//! **Rate limit.** Every answer's `X-RateLimit-Remaining` and
//! `X-RateLimit-Reset` (and `Retry-After`) are read. When none is left the
//! service waits for the reset (at most [`MAX_WAIT`]) and fetches answer
//! [`FetchError::RateLimited`] without a request until then. What is
//! remembered (the wait, the login, the denied parts) is kept in memory, per
//! service, for at most [`MEMORY_MAX`] services, and dropped when the token
//! changes.
//!
//! Pull request titles and repository names are never kept: the reading holds
//! numbers only. The token is sent as `Authorization: Bearer`, marked
//! sensitive, and never reaches an error, a reading or a log line.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use reqwest::header::{ACCEPT, HeaderMap};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use url::Url;

use super::api;
use super::{
    BoxFuture, FetchCtx, FetchError, Field, FieldKind, Format, Integration, Level, MetricDef, Need,
    SettingError, Snapshot, Status, Value, read_capped,
};

/// The address of the GitHub API.
pub const API: &str = "https://api.github.com";

/// The most users and organisations the `owners` setting may name.
pub const MAX_OWNERS: usize = 5;
/// The most repositories a fetch asks the workflow runs of (REST).
pub const CI_FALLBACK_MAX: usize = 10;
/// With this few requests left in the window the optional REST calls wait.
const LOW_REMAINING: u64 = 25;
/// How long a service waits after a secondary rate limit (no reset is given).
const SECONDARY_WAIT: SignedDuration = SignedDuration::from_secs(60);
/// The longest a service waits for a rate limit to reset.
pub const MAX_WAIT: SignedDuration = SignedDuration::from_hours(2);
/// How long a part of the query that the token may not read is left out.
const DENIED_FOR: SignedDuration = SignedDuration::from_hours(1);
/// Most services whose memory is kept; past that the oldest goes.
pub const MEMORY_MAX: usize = 256;
/// How many times one fetch asks again after leaving out a denied part.
const ATTEMPTS: usize = 3;

/// The only instance.
pub static GITHUB: GithubKind = GithubKind { base: None };

/// See the module.
pub struct GithubKind {
    /// Another API address, for tests; `None` is [`API`].
    base: Option<String>,
}

impl GithubKind {
    /// A kind that talks to `base` instead of GitHub (a stub in a test). No
    /// service has a way to choose it.
    pub fn with_base(base: impl Into<String>) -> Self {
        GithubKind {
            base: Some(base.into()),
        }
    }
}

static FIELDS: [Field; 2] = [
    Field {
        key: "owners",
        label_key: "field.github_owners",
        help_key: "field.github_owners_help",
        placeholder: "my-org",
        kind: FieldKind::Text,
        required: false,
        secret: false,
    },
    Field {
        key: "secret",
        label_key: "field.github_secret",
        help_key: "field.github_secret_help",
        placeholder: "github_pat_…",
        kind: FieldKind::Text,
        required: true,
        secret: true,
    },
];

static METRICS: [MetricDef; 5] = [
    MetricDef {
        id: "review_requests",
        label_key: "metric.gh_review_requests",
        format: Format::Number,
    },
    MetricDef {
        id: "ci_red",
        label_key: "metric.gh_ci_red",
        format: Format::Number,
    },
    MetricDef {
        id: "dependabot",
        label_key: "metric.gh_dependabot",
        format: Format::Number,
    },
    MetricDef {
        id: "releases_24h",
        label_key: "metric.gh_releases_24h",
        format: Format::Number,
    },
    MetricDef {
        id: "open_prs",
        label_key: "metric.gh_open_prs",
        format: Format::Number,
    },
];

impl Integration for GithubKind {
    fn kind(&self) -> &'static str {
        "github"
    }

    fn label_key(&self) -> &'static str {
        "kind.github"
    }

    /// The address a card opens: GitHub's site, not the API's.
    fn default_link(&self) -> Option<&'static str> {
        Some("https://github.com/")
    }

    fn fields(&self) -> &'static [Field] {
        &FIELDS
    }

    fn metrics(&self) -> &'static [MetricDef] {
        &METRICS
    }

    fn default_size(&self) -> crate::layout::Size {
        crate::layout::Size::Wide
    }
    fn default_interval(&self) -> Duration {
        Duration::from_secs(5 * 60)
    }

    fn min_interval(&self) -> Duration {
        Duration::from_secs(2 * 60)
    }

    fn url_required(&self) -> bool {
        false
    }

    fn personal(&self) -> bool {
        true
    }

    fn check_settings(&self, settings: &serde_json::Map<String, Json>) -> Result<(), SettingError> {
        match settings.get("owners") {
            None => Ok(()),
            Some(Json::String(text)) => parse_owners(text).map(|_| ()).ok_or(SettingError {
                field: "owners",
                message_key: "err.github_owners",
            }),
            Some(_) => Err(SettingError {
                field: "owners",
                message_key: "err.github_owners",
            }),
        }
    }

    fn fetch<'a>(&'a self, ctx: &'a FetchCtx) -> BoxFuture<'a, Result<Snapshot, FetchError>> {
        Box::pin(async move {
            let auth = api::Credential::bearer(ctx.secret.as_deref())?;
            let owners = match ctx.settings.get("owners") {
                None | Some(Json::Null) => Vec::new(),
                Some(Json::String(text)) => parse_owners(text).ok_or(FetchError::Config)?,
                Some(_) => return Err(FetchError::Config),
            };
            let base =
                Url::parse(self.base.as_deref().unwrap_or(API)).map_err(|_| FetchError::Config)?;
            let fingerprint = fingerprint(ctx.secret.as_deref().unwrap_or_default());
            let session = Session {
                http: &ctx.http,
                base,
                auth,
                memory: ctx.service_id().map(|id| (id, fingerprint)),
            };
            session.run(&owners, Timestamp::now()).await
        })
    }
}

// ---------------------------------------------------------------------------
// The setting

/// The users and organisations an `owners` setting names: separated by commas
/// or spaces, each a GitHub login (letters, digits and single hyphens inside,
/// at most 39 characters), at most [`MAX_OWNERS`], each once (case ignored).
/// `None` for anything else.
pub fn parse_owners(text: &str) -> Option<Vec<String>> {
    let mut owners: Vec<String> = Vec::new();
    for name in text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|n| !n.is_empty())
    {
        if !is_login(name) {
            return None;
        }
        if !owners.iter().any(|o| o.eq_ignore_ascii_case(name)) {
            owners.push(name.to_string());
        }
    }
    if owners.len() > MAX_OWNERS {
        return None;
    }
    Some(owners)
}

fn is_login(name: &str) -> bool {
    (1..=39).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// A repository name as it may go into a path: letters, digits, `.`, `_`, `-`.
fn is_repo_name(name: &str) -> bool {
    (1..=100).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && name != "."
        && name != ".."
}

// ---------------------------------------------------------------------------
// What is remembered per service

/// A part of the query a token may not be allowed to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Rollup,
    Alerts,
    Releases,
    Search,
}

impl Part {
    const ALL: [Part; 4] = [Part::Rollup, Part::Alerts, Part::Releases, Part::Search];

    /// The field of the query that names this part in an error's path.
    fn fields(self) -> &'static [&'static str] {
        match self {
            Part::Rollup => &["statusCheckRollup", "defaultBranchRef"],
            Part::Alerts => &["vulnerabilityAlerts"],
            Part::Releases => &["releases"],
            Part::Search => &["reviews", "authored"],
        }
    }
}

/// Which parts are left out, until when.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Denied([Option<Timestamp>; 4]);

impl Denied {
    fn is(&self, part: Part, now: Timestamp) -> bool {
        self.0[part as usize].is_some_and(|until| until > now)
    }

    fn deny(&mut self, part: Part, now: Timestamp) {
        self.0[part as usize] = now.checked_add(DENIED_FOR).ok();
    }
}

#[derive(Debug)]
struct Entry {
    id: i64,
    /// Which token this is for: a new token starts afresh.
    fingerprint: [u8; 8],
    login: Option<String>,
    blocked_until: Option<Timestamp>,
    denied: Denied,
}

/// What the kind keeps between two fetches of a service. Bounded by
/// [`MEMORY_MAX`], oldest first out. Nothing in it is a secret (the
/// fingerprint is a few bytes of a hash).
#[derive(Debug, Default)]
struct Memory {
    entries: VecDeque<Entry>,
}

impl Memory {
    const fn new() -> Self {
        Memory {
            entries: VecDeque::new(),
        }
    }

    /// The entry of service `id` for the token `fingerprint`: made when it is
    /// new, and made afresh when the token is another.
    fn entry(&mut self, id: i64, fingerprint: [u8; 8]) -> &mut Entry {
        let at = self.entries.iter().position(|e| e.id == id);
        let fresh = || Entry {
            id,
            fingerprint,
            login: None,
            blocked_until: None,
            denied: Denied::default(),
        };
        match at {
            Some(at) => {
                if self.entries[at].fingerprint != fingerprint {
                    self.entries[at] = fresh();
                }
                &mut self.entries[at]
            }
            None => {
                while self.entries.len() >= MEMORY_MAX {
                    self.entries.pop_front();
                }
                self.entries.push_back(fresh());
                self.entries.back_mut().expect("just pushed")
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

fn memory() -> std::sync::MutexGuard<'static, Memory> {
    static MEMORY: Mutex<Memory> = Mutex::new(Memory::new());
    // A panic elsewhere cannot leave the list in a state that matters.
    MEMORY.lock().unwrap_or_else(|e| e.into_inner())
}

fn fingerprint(secret: &str) -> [u8; 8] {
    let digest = Sha256::digest(secret.as_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

// ---------------------------------------------------------------------------
// The rate limit

/// What an answer says about the request limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Limits {
    remaining: Option<u64>,
    reset: Option<Timestamp>,
    retry_after: Option<u64>,
}

impl Limits {
    fn of(headers: &HeaderMap) -> Limits {
        let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        Limits {
            remaining: text("x-ratelimit-remaining").and_then(|v| v.trim().parse().ok()),
            reset: text("x-ratelimit-reset")
                .and_then(|v| v.trim().parse::<i64>().ok())
                .and_then(|s| Timestamp::from_second(s).ok()),
            retry_after: text("retry-after").and_then(|v| v.trim().parse().ok()),
        }
    }

    /// Nothing is left of the window (or GitHub asked to wait).
    fn exhausted(&self) -> bool {
        self.remaining == Some(0) || self.retry_after.is_some()
    }

    /// Until when to leave the service alone, if the limit is used up: the
    /// reset (or the `Retry-After`), never more than [`MAX_WAIT`] from `now`
    /// and at least a second.
    fn wait_until(&self, now: Timestamp) -> Option<Timestamp> {
        if !self.exhausted() {
            return None;
        }
        let latest = now.checked_add(MAX_WAIT).unwrap_or(now);
        let soonest = now.checked_add(SignedDuration::from_secs(1)).unwrap_or(now);
        let target = match (self.retry_after, self.reset) {
            (Some(secs), _) => i64::try_from(secs)
                .ok()
                .and_then(|s| now.checked_add(SignedDuration::from_secs(s)).ok()),
            (None, Some(reset)) => Some(reset),
            // No reset given: a minute.
            (None, None) => now.checked_add(SignedDuration::from_secs(60)).ok(),
        }
        .unwrap_or(latest);
        Some(target.clamp(soonest, latest))
    }
}

// ---------------------------------------------------------------------------
// One fetch

struct Session<'a> {
    http: &'a reqwest::Client,
    base: Url,
    auth: api::Credential,
    /// The service and its token, when the service is a stored one (what is
    /// remembered is kept under them).
    memory: Option<(i64, [u8; 8])>,
}

/// One repository as the answer gives it. Names only live for the fetch.
#[derive(Debug, Default, Clone)]
struct Repo {
    name: String,
    default_branch: Option<String>,
    /// The state of the default branch's checks, when the answer has one.
    rollup: Option<String>,
    alerts: Option<f64>,
    /// Releases published in the last day; `None` when the answer had no
    /// releases for the repository.
    releases_24h: Option<f64>,
}

/// What the query's answer holds.
#[derive(Debug, Default)]
struct Answer {
    /// Whether the answer had a `data` object at all.
    had_data: bool,
    reviews: Option<f64>,
    authored: Option<f64>,
    /// Whether any list of repositories came back (an empty one counts).
    repos_seen: bool,
    repos: Vec<Repo>,
    /// The parts the answer's errors name, and whether the error says the
    /// token may not read them (lasting) or is only a failure of this answer.
    denied: Vec<(Part, bool)>,
}

impl Session<'_> {
    fn with_entry<T>(&self, f: impl FnOnce(&mut Entry) -> T) -> Option<T> {
        let (id, fp) = self.memory?;
        Some(f(memory().entry(id, fp)))
    }

    fn blocked_until(&self, now: Timestamp) -> Option<Timestamp> {
        self.with_entry(|e| e.blocked_until)
            .flatten()
            .filter(|until| *until > now)
    }

    /// Remembers what an answer said about the limit.
    fn note(&self, limits: &Limits, now: Timestamp) {
        if let Some(until) = limits.wait_until(now) {
            self.with_entry(|e| e.blocked_until = Some(until));
        }
    }

    async fn run(&self, owners: &[String], now: Timestamp) -> Result<Snapshot, FetchError> {
        if self.blocked_until(now).is_some() {
            return Err(FetchError::RateLimited);
        }
        let login = match self.with_entry(|e| e.login.clone()).flatten() {
            Some(login) => Some(login),
            None => {
                let login = self.login().await?;
                if let Some(login) = &login {
                    self.with_entry(|e| e.login = Some(login.clone()));
                }
                login
            }
        };
        // What is left out for good (an hour), and what is also left out of
        // this fetch's later attempts (a part that failed for another reason).
        let mut stored = self.with_entry(|e| e.denied).unwrap_or_default();
        let mut denied = stored;
        let mut remaining = None;
        let mut answer = Answer::default();
        for attempt in 1..=ATTEMPTS {
            let (parsed, limits) = self.query(owners, login.as_deref(), &denied, now).await?;
            remaining = limits.remaining;
            let new: Vec<(Part, bool)> = parsed
                .denied
                .iter()
                .copied()
                .filter(|(p, _)| !denied.is(*p, now))
                .collect();
            answer = parsed;
            if new.is_empty() || attempt == ATTEMPTS {
                break;
            }
            for (part, lasting) in new {
                denied.deny(part, now);
                if lasting {
                    stored.deny(part, now);
                }
            }
        }
        self.with_entry(|e| e.denied = stored);

        if !answer.had_data {
            return Err(FetchError::Invalid);
        }
        let ci = self.ci(&answer.repos, remaining, now).await;
        let snapshot = reading(&answer, &ci);
        if snapshot.values.is_empty() {
            // Nothing in the answer could be a number.
            return Err(FetchError::Invalid);
        }
        Ok(snapshot)
    }

    /// The user's login, asked once.
    async fn login(&self) -> Result<Option<String>, FetchError> {
        let body = json!({ "query": "query { viewer { login } }" });
        let (root, _) = self.graphql(&body, Timestamp::now()).await?;
        Ok(root
            .pointer("/data/viewer/login")
            .and_then(Json::as_str)
            .filter(|login| is_login(login))
            .map(str::to_string))
    }

    /// One GraphQL query: the numbers, and which parts were refused.
    async fn query(
        &self,
        owners: &[String],
        login: Option<&str>,
        denied: &Denied,
        now: Timestamp,
    ) -> Result<(Answer, Limits), FetchError> {
        let body = build_query(owners, login, denied, now);
        let (root, limits) = self.graphql(&body, now).await?;
        let data = root.get("data").filter(|d| d.is_object());
        let mut answer = Answer {
            denied: denied_parts(&root),
            ..Answer::default()
        };
        let Some(data) = data else {
            // No data at all: a refused part that takes the whole answer
            // with it is left out next time; anything else is no reading.
            if answer.denied.is_empty() {
                return Err(FetchError::Invalid);
            }
            return Ok((answer, limits));
        };
        answer.had_data = true;
        answer.reviews = api::count(data.pointer("/reviews/issueCount"));
        answer.authored = api::count(data.pointer("/authored/issueCount"));
        let mut keys: Vec<String> = vec!["viewer".to_string()];
        keys.extend((0..owners.len()).map(|i| format!("o{i}")));
        let mut names = BTreeSet::new();
        for key in keys {
            let Some(nodes) = data
                .get(&key)
                .and_then(|o| o.pointer("/repositories/nodes"))
                .and_then(Json::as_array)
            else {
                continue;
            };
            answer.repos_seen = true;
            for node in nodes {
                if let Some(repo) = repo_of(node, now)
                    && names.insert(repo.name.clone())
                {
                    answer.repos.push(repo);
                }
            }
        }
        Ok((answer, limits))
    }

    /// One POST to the GraphQL endpoint: the answer as JSON and its limits.
    async fn graphql(&self, body: &Json, now: Timestamp) -> Result<(Json, Limits), FetchError> {
        let url = api::endpoint(&self.base, &["graphql"], None)?;
        let request = self.http.post(url).json(body);
        let (bytes, limits) = self.send(request, now).await?;
        let root: Json = serde_json::from_slice(&bytes).map_err(|_| FetchError::Invalid)?;
        // A GraphQL answer can say "rate limited" with a 200.
        let limited = root
            .get("errors")
            .and_then(Json::as_array)
            .is_some_and(|errors| {
                errors
                    .iter()
                    .any(|e| e.get("type").and_then(Json::as_str) == Some("RATE_LIMITED"))
            });
        if limited {
            let exhausted = Limits {
                remaining: Some(0),
                ..limits
            };
            self.note(&exhausted, now);
            return Err(FetchError::RateLimited);
        }
        Ok((root, limits))
    }

    /// One request with the token; the body and what the answer says about the
    /// limit. 401 is [`FetchError::Auth`]; 403 and 429 are
    /// [`FetchError::RateLimited`] when the limit is used up, else 403 is
    /// [`FetchError::Auth`] and 429 a status.
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        now: Timestamp,
    ) -> Result<(Vec<u8>, Limits), FetchError> {
        // With the token on it: a redirect to another origin is refused.
        let request = self
            .auth
            .apply(request)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        let response = api::send_credentialed(request).await?;
        let code = response.status().as_u16();
        let limits = Limits::of(response.headers());
        self.note(&limits, now);
        if matches!(code, 403 | 429) && !limits.exhausted() {
            // A secondary limit says so in the body and gives no window: wait
            // a minute. Anything else is a refusal (403) or a status (429).
            let body = read_capped(response).await.unwrap_or_default();
            if is_secondary_limit(&body) {
                let until = now.checked_add(SECONDARY_WAIT).unwrap_or(now);
                self.with_entry(|e| e.blocked_until = Some(until));
                return Err(FetchError::RateLimited);
            }
            return Err(if code == 403 {
                FetchError::Auth
            } else {
                FetchError::Status(code)
            });
        }
        match code {
            401 => Err(FetchError::Auth),
            403 | 429 => Err(FetchError::RateLimited),
            code if !(200..300).contains(&code) => Err(FetchError::Status(code)),
            _ => Ok((read_capped(response).await?, limits)),
        }
    }

    /// Which repositories are red: those whose default branch's checks failed
    /// in the answer, and, for those the answer says nothing about, those whose
    /// newest workflow runs failed (one call each, at most
    /// [`CI_FALLBACK_MAX`]). `None` for a repository nothing could be said
    /// about.
    async fn ci(
        &self,
        repos: &[Repo],
        remaining: Option<u64>,
        now: Timestamp,
    ) -> Vec<Option<bool>> {
        let mut out: Vec<Option<bool>> = repos
            .iter()
            .map(|repo| match repo.rollup.as_deref() {
                Some("FAILURE" | "ERROR") => Some(true),
                Some("SUCCESS" | "PENDING" | "EXPECTED") => Some(false),
                _ => None,
            })
            .collect();
        let spare = remaining.is_none_or(|r| r > LOW_REMAINING);
        if !spare || self.blocked_until(now).is_some() {
            return out;
        }
        let wanted: Vec<usize> = repos
            .iter()
            .enumerate()
            .filter(|(i, repo)| out[*i].is_none() && repo.default_branch.is_some())
            .map(|(i, _)| i)
            .take(CI_FALLBACK_MAX)
            .collect();
        let runs = futures_util::future::join_all(
            wanted.iter().map(|i| self.workflow_red(&repos[*i], now)),
        )
        .await;
        for (i, red) in wanted.into_iter().zip(runs) {
            out[i] = red;
        }
        out
    }

    /// Whether the newest completed run of any workflow on the default branch
    /// failed. `None` when it could not be read (no permission, an error).
    async fn workflow_red(&self, repo: &Repo, now: Timestamp) -> Option<bool> {
        let (owner, name) = repo.name.split_once('/')?;
        let branch = repo.default_branch.as_deref()?;
        if !is_login(owner) || !is_repo_name(name) {
            return None;
        }
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("branch", branch)
            .append_pair("status", "completed")
            .append_pair("per_page", "10")
            .append_pair("exclude_pull_requests", "true")
            .finish();
        let url = api::endpoint(
            &self.base,
            &["repos", owner, name, "actions", "runs"],
            Some(&query),
        )
        .ok()?;
        let (bytes, _) = self.send(self.http.get(url), now).await.ok()?;
        let root: Json = serde_json::from_slice(&bytes).ok()?;
        let runs = root.get("workflow_runs")?.as_array()?;
        Some(newest_runs_failed(runs))
    }
}

/// Whether the newest informative run of any workflow in `runs` failed. A
/// run that was cancelled, skipped or neither passed nor failed says nothing
/// about the workflow.
fn newest_runs_failed(runs: &[Json]) -> bool {
    let mut newest: Vec<(i64, i64, bool)> = Vec::new();
    for run in runs {
        let workflow = run.get("workflow_id").and_then(Json::as_i64).unwrap_or(0);
        let id = run.get("id").and_then(Json::as_i64).unwrap_or(0);
        let red = match run.get("conclusion").and_then(Json::as_str) {
            Some("failure" | "timed_out") => true,
            Some("success") => false,
            _ => continue,
        };
        match newest.iter_mut().find(|(w, _, _)| *w == workflow) {
            Some(known) if known.1 < id => *known = (workflow, id, red),
            Some(_) => {}
            None => newest.push((workflow, id, red)),
        }
    }
    newest.iter().any(|(_, _, red)| *red)
}

// ---------------------------------------------------------------------------
// The query and its answer

const REPOSITORY: &str = "fragment R on Repository {
  nameWithOwner
  defaultBranchRef { name target { ... on Commit { statusCheckRollup { state } } } }
  vulnerabilityAlerts(first: 1, states: OPEN) { totalCount }
  releases(first: 3, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { publishedAt isDraft } }
}";

/// The query and its variables. Owners and the login only ever travel as
/// variables. A denied part is left out (its field stays, without the
/// sub-selection that needs the permission).
fn build_query(owners: &[String], login: Option<&str>, denied: &Denied, now: Timestamp) -> Json {
    let repositories = "repositories(first: 100, ownerAffiliations: [OWNER], isArchived: false, \
                        isFork: false, orderBy: {field: PUSHED_AT, direction: DESC}) \
                        { nodes { ...R } }";
    let mut declared: Vec<String> = Vec::new();
    let mut variables = serde_json::Map::new();
    let mut selections: Vec<String> = vec!["rateLimit { cost remaining resetAt }".to_string()];
    if owners.is_empty() {
        selections.push(
            "viewer { repositories(first: 100, ownerAffiliations: [OWNER], isArchived: false, \
             isFork: false, orderBy: {field: PUSHED_AT, direction: DESC}) { nodes { ...R } } }"
                .to_string(),
        );
    }
    for (i, owner) in owners.iter().enumerate() {
        declared.push(format!("$o{i}: String!"));
        variables.insert(format!("o{i}"), Json::String(owner.clone()));
        selections.push(format!(
            "o{i}: repositoryOwner(login: $o{i}) {{ {repositories} }}"
        ));
    }
    if let Some(login) = login.filter(|_| !denied.is(Part::Search, now)) {
        let scope: String = owners.iter().map(|o| format!(" user:{o}")).collect();
        declared.push("$reviewQ: String!".to_string());
        declared.push("$authoredQ: String!".to_string());
        variables.insert(
            "reviewQ".into(),
            Json::String(format!(
                "is:pr is:open review-requested:{login} archived:false{scope}"
            )),
        );
        variables.insert(
            "authoredQ".into(),
            Json::String(format!(
                "is:pr is:open author:{login} archived:false{scope}"
            )),
        );
        selections
            .push("reviews: search(query: $reviewQ, type: ISSUE, first: 1) { issueCount }".into());
        selections.push(
            "authored: search(query: $authoredQ, type: ISSUE, first: 1) { issueCount }".into(),
        );
    }
    let mut fragment = REPOSITORY.to_string();
    if denied.is(Part::Rollup, now) {
        fragment = fragment.replace(
            "defaultBranchRef { name target { ... on Commit { statusCheckRollup { state } } } }",
            "defaultBranchRef { name }",
        );
    }
    if denied.is(Part::Alerts, now) {
        fragment = fragment.replace(
            "  vulnerabilityAlerts(first: 1, states: OPEN) { totalCount }\n",
            "",
        );
    }
    if denied.is(Part::Releases, now) {
        fragment = fragment.replace(
            "  releases(first: 3, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { publishedAt isDraft } }\n",
            "",
        );
    }
    let declared = if declared.is_empty() {
        String::new()
    } else {
        format!("({})", declared.join(", "))
    };
    json!({
        "query": format!("query Magpie{declared} {{ {} }} {fragment}", selections.join(" ")),
        "variables": variables,
    })
}

/// Whether a refusal's body says GitHub's secondary rate limit was hit.
fn is_secondary_limit(body: &[u8]) -> bool {
    String::from_utf8_lossy(body)
        .to_ascii_lowercase()
        .contains("secondary rate limit")
}

/// The parts the errors of an answer name: the ones whose field is in an
/// error's path. An error of type FORBIDDEN or INSUFFICIENT_SCOPES says the
/// token may not read the part (`true`: lasting); any other error with a path
/// is a failure of this answer only (`false`).
fn denied_parts(root: &Json) -> Vec<(Part, bool)> {
    let Some(errors) = root.get("errors").and_then(Json::as_array) else {
        return Vec::new();
    };
    Part::ALL
        .into_iter()
        .filter_map(|part| {
            let naming: Vec<&Json> = errors
                .iter()
                .filter(|error| {
                    error
                        .get("path")
                        .and_then(Json::as_array)
                        .is_some_and(|path| {
                            path.iter()
                                .filter_map(Json::as_str)
                                .any(|step| part.fields().contains(&step))
                        })
                })
                .collect();
            let lasting = naming.iter().any(|error| {
                matches!(
                    error.get("type").and_then(Json::as_str),
                    Some("FORBIDDEN" | "INSUFFICIENT_SCOPES")
                )
            });
            (!naming.is_empty()).then_some((part, lasting))
        })
        .collect()
}

/// A repository from its node, if it is one; `None` for a null or odd node.
fn repo_of(node: &Json, now: Timestamp) -> Option<Repo> {
    let name = node.get("nameWithOwner")?.as_str()?;
    let branch = node.get("defaultBranchRef").filter(|b| b.is_object());
    let day_ago = now.checked_sub(SignedDuration::from_hours(24)).ok()?;
    let releases_24h = node
        .pointer("/releases/nodes")
        .and_then(Json::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter(|r| r.get("isDraft").and_then(Json::as_bool) == Some(false))
                .filter_map(|r| r.get("publishedAt")?.as_str()?.parse::<Timestamp>().ok())
                .filter(|at| *at >= day_ago && *at <= now)
                .count() as f64
        });
    Some(Repo {
        name: name.to_string(),
        default_branch: branch
            .and_then(|b| b.get("name"))
            .and_then(Json::as_str)
            .map(str::to_string),
        rollup: branch
            .and_then(|b| b.pointer("/target/statusCheckRollup/state"))
            .and_then(Json::as_str)
            .map(str::to_string),
        alerts: api::count(node.pointer("/vulnerabilityAlerts/totalCount")),
        releases_24h,
    })
}

/// The reading from what was found.
fn reading(answer: &Answer, ci: &[Option<bool>]) -> Snapshot {
    let mut snapshot = Snapshot::new(Status::Ok);
    let mut put = |id: &str, n: Option<f64>| {
        if let Some(n) = n {
            snapshot.values.insert(id.to_string(), Value::num(n));
        }
    };
    put("review_requests", answer.reviews);
    put("open_prs", answer.authored);

    // A metric over the repositories is there when at least one repository
    // could say something (or there are none to ask).
    if answer.repos_seen {
        let known = ci.iter().flatten().count();
        if answer.repos.is_empty() || known > 0 {
            put(
                "ci_red",
                Some(ci.iter().filter(|red| **red == Some(true)).count() as f64),
            );
        }
        let alerts: Vec<f64> = answer.repos.iter().filter_map(|r| r.alerts).collect();
        if answer.repos.is_empty() || !alerts.is_empty() {
            put("dependabot", Some(alerts.iter().sum()));
        }
        let releases: Vec<f64> = answer.repos.iter().filter_map(|r| r.releases_24h).collect();
        if answer.repos.is_empty() || !releases.is_empty() {
            put("releases_24h", Some(releases.iter().sum()));
        }
    }

    let number = |id: &str| {
        snapshot
            .values
            .get(id)
            .and_then(|v| v.num)
            .unwrap_or_default() as usize
    };
    let (review, red) = (number("review_requests"), number("ci_red"));
    let mut needs = Vec::new();
    if red > 0 {
        needs.push(need("gh:ci_red", Level::Bad, "need.gh_ci_red", red));
    }
    if review > 0 {
        needs.push(need("gh:review", Level::Warn, "need.gh_review", review));
    }
    snapshot.status = if red > 0 {
        Status::Bad
    } else if review > 0 {
        Status::Warn
    } else {
        Status::Ok
    };
    snapshot.needs = needs;
    snapshot
}

/// A need whose only parameter is how many: no title and no repository name
/// from GitHub goes in.
fn need(key: &str, level: Level, label_key: &str, count: usize) -> Need {
    Need {
        key: key.to_string(),
        level,
        label_key: label_key.to_string(),
        params: [("count".to_string(), count.to_string())].into(),
        score: count as f64,
        since: None,
    }
}

#[cfg(test)]
mod tests {
    use reqwest::header::HeaderValue;

    use super::*;

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).unwrap()
    }

    #[test]
    fn owners_are_logins_split_by_commas_or_spaces() {
        assert_eq!(parse_owners(""), Some(vec![]));
        assert_eq!(parse_owners(" , "), Some(vec![]));
        assert_eq!(
            parse_owners("Ferdinand99, my-org  other"),
            Some(vec!["Ferdinand99".into(), "my-org".into(), "other".into()])
        );
        // Each once, whatever the case.
        assert_eq!(parse_owners("a,A,b"), Some(vec!["a".into(), "b".into()]));
        for bad in [
            "-a",
            "a-",
            "a--b",
            "a/b",
            "a b/c",
            "a:b",
            "a\"b",
            "user:evil",
            "é",
            &"a".repeat(40),
            "a,b,c,d,e,f",
        ] {
            assert_eq!(parse_owners(bad), None, "{bad:?}");
        }
        assert!(parse_owners(&"a".repeat(39)).is_some());
    }

    #[test]
    fn repository_names_are_safe_in_a_path() {
        for ok in ["repo", "my.repo_1-x", ".github"] {
            assert!(is_repo_name(ok), "{ok}");
        }
        for bad in ["", ".", "..", "a/b", "a b", "a?b", "a#b", "é"] {
            assert!(!is_repo_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn memory_is_bounded_and_a_new_token_starts_afresh() {
        let mut memory = Memory::new();
        let fp = [1u8; 8];
        memory.entry(1, fp).login = Some("me".into());
        assert_eq!(memory.entry(1, fp).login.as_deref(), Some("me"));
        // Another token for the same service: nothing carries over.
        assert_eq!(memory.entry(1, [2u8; 8]).login, None);
        for id in 0..(MEMORY_MAX as i64 + 10) {
            memory.entry(1000 + id, fp);
        }
        assert_eq!(memory.len(), MEMORY_MAX);
        // The oldest went first.
        assert!(memory.entries.iter().all(|e| e.id != 1));
        assert!(
            memory
                .entries
                .iter()
                .any(|e| e.id == 1000 + MEMORY_MAX as i64 + 9)
        );
    }

    #[test]
    fn the_limit_is_read_from_the_headers_and_waits_for_the_reset() {
        let now = at(1_000_000);
        let headers = |pairs: &[(&'static str, &str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                map.insert(*name, HeaderValue::from_str(value).unwrap());
            }
            map
        };
        let limits = Limits::of(&headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1000600"),
        ]));
        assert!(limits.exhausted());
        assert_eq!(limits.wait_until(now), Some(at(1_000_600)));
        // Some left: no wait.
        let limits = Limits::of(&headers(&[
            ("x-ratelimit-remaining", "4999"),
            ("x-ratelimit-reset", "1000600"),
        ]));
        assert!(!limits.exhausted());
        assert_eq!(limits.wait_until(now), None);
        // A reset in the past, or far away, is brought into range.
        let past = Limits::of(&headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "999000"),
        ]));
        assert_eq!(past.wait_until(now), Some(at(1_000_001)));
        let far = Limits::of(&headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "99999999"),
        ]));
        assert_eq!(far.wait_until(now), Some(at(1_000_000 + 7200)));
        // Retry-After wins, and odd values wait for nothing special.
        let retry = Limits::of(&headers(&[("retry-after", "90")]));
        assert_eq!(retry.wait_until(now), Some(at(1_000_090)));
        let odd = Limits::of(&headers(&[("x-ratelimit-remaining", "lots")]));
        assert_eq!(odd, Limits::default());
        assert_eq!(odd.wait_until(now), None);
    }

    #[test]
    fn a_denied_part_is_left_out_for_an_hour() {
        let now = at(1_000_000);
        let mut denied = Denied::default();
        assert!(!denied.is(Part::Alerts, now));
        denied.deny(Part::Alerts, now);
        assert!(denied.is(Part::Alerts, now));
        assert!(!denied.is(Part::Releases, now));
        assert!(!denied.is(Part::Alerts, at(1_000_000 + 3600)));
        let query = build_query(&[], Some("me"), &denied, now);
        let text = query["query"].as_str().unwrap();
        assert!(!text.contains("vulnerabilityAlerts"), "{text}");
        assert!(text.contains("releases("), "{text}");
        let full = build_query(&[], Some("me"), &Denied::default(), now);
        assert!(
            full["query"]
                .as_str()
                .unwrap()
                .contains("vulnerabilityAlerts")
        );
    }

    #[test]
    fn owners_and_the_login_travel_as_variables_only() {
        let now = at(1_000_000);
        let owners = vec!["acme".to_string(), "me".to_string()];
        let query = build_query(&owners, Some("me"), &Denied::default(), now);
        let text = query["query"].as_str().unwrap();
        assert!(!text.contains("acme"), "{text}");
        assert!(!text.contains("review-requested"), "{text}");
        assert_eq!(query["variables"]["o0"], "acme");
        assert_eq!(query["variables"]["o1"], "me");
        let review = query["variables"]["reviewQ"].as_str().unwrap();
        assert_eq!(
            review,
            "is:pr is:open review-requested:me archived:false user:acme user:me"
        );
        // No login yet: no search.
        let query = build_query(&owners, None, &Denied::default(), now);
        assert!(!query["query"].as_str().unwrap().contains("search("));
    }

    #[test]
    fn the_newest_run_of_each_workflow_decides() {
        let run = |id: i64, workflow: i64, conclusion: &str| json!({ "id": id, "workflow_id": workflow, "conclusion": conclusion });
        // The newest of workflow 1 passed, an older one failed.
        assert!(!newest_runs_failed(&[
            run(5, 1, "success"),
            run(2, 1, "failure")
        ]));
        // Order in the list does not matter.
        assert!(!newest_runs_failed(&[
            run(2, 1, "failure"),
            run(5, 1, "success")
        ]));
        // One red workflow is enough.
        assert!(newest_runs_failed(&[
            run(5, 1, "success"),
            run(4, 2, "timed_out")
        ]));
        // A cancelled run says nothing: the failure before it still stands.
        assert!(newest_runs_failed(&[
            run(6, 1, "cancelled"),
            run(2, 1, "failure")
        ]));
        assert!(!newest_runs_failed(&[]));
    }
}
