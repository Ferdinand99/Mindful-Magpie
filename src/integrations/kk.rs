//! The `kk` kind («Keen Kestrel»): the owner's error tracking and analytics
//! app, read through `GET /api/v1/summary?hours=24` with a read token.
//!
//! One call gives what the card shows: the issues that are new, regressed or
//! unresolved, crash-free sessions and today's visitors and app users, added
//! up over every project, plus each project's newest issue. The endpoint
//! shipped in Keen Kestrel 0.10.0; an older server answers 404, which this
//! kind turns into [`FetchError::KkTooOld`] and does not ask again for an
//! hour.
//!
//! The token is sent as `Authorization: Bearer` and is marked sensitive, so
//! it stays out of debug output. Nothing here puts a token, an address or a
//! response text into an error. The only text taken from the answer is the
//! newest issue's title, and it goes into a `text` metric only: needs carry
//! a count.

use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde_json::{Map, Value as Json};
use url::Url;
use vole_kit::net::check_host;

use super::{
    BoxFuture, FetchCtx, FetchError, Field, FieldKind, Format, Integration, Level, MetricDef, Need,
    Snapshot, Status, Value, classify, read_capped,
};

/// The only instance.
pub static KK: KkKind = KkKind;

/// See the module.
pub struct KkKind;

/// The window the summary is asked for.
const HOURS: &str = "24";
/// How long a server that answered 404 is left alone.
const REMEMBER_OLD: Duration = Duration::from_secs(60 * 60);
/// How many addresses are remembered at most (an admin can only add so many).
const MAX_REMEMBERED: usize = 256;
/// The longest newest-issue title kept, in characters.
const MAX_TITLE: usize = 200;

static FIELDS: [Field; 1] = [Field {
    key: "secret",
    label_key: "field.kk_secret",
    help_key: "field.kk_secret_help",
    placeholder: "",
    kind: FieldKind::Text,
    required: true,
    secret: true,
}];

static METRICS: [MetricDef; 7] = [
    MetricDef {
        id: "new_issues",
        label_key: "metric.kk_new_issues",
        format: Format::Number,
    },
    MetricDef {
        id: "regressed_issues",
        label_key: "metric.kk_regressed_issues",
        format: Format::Number,
    },
    MetricDef {
        id: "unresolved_issues",
        label_key: "metric.kk_unresolved_issues",
        format: Format::Number,
    },
    MetricDef {
        id: "crash_free",
        label_key: "metric.kk_crash_free",
        format: Format::Percent,
    },
    MetricDef {
        id: "visitors_today",
        label_key: "metric.kk_visitors_today",
        format: Format::Number,
    },
    MetricDef {
        id: "app_users_today",
        label_key: "metric.kk_app_users_today",
        format: Format::Number,
    },
    MetricDef {
        id: "newest_issue",
        label_key: "metric.kk_newest_issue",
        format: Format::Text,
    },
];

/// The servers that answered 404, and until when they are left alone. The key
/// is the summary address; it never leaves memory.
struct OldServers {
    until: Mutex<HashMap<String, Instant>>,
}

impl OldServers {
    fn new() -> Self {
        OldServers {
            until: Mutex::new(HashMap::new()),
        }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Instant>> {
        self.until.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether `key` answered 404 less than an hour before `now`.
    fn is_old(&self, key: &str, now: Instant) -> bool {
        let mut map = self.map();
        match map.get(key) {
            Some(until) if now < *until => true,
            Some(_) => {
                map.remove(key);
                false
            }
            None => false,
        }
    }

    fn mark(&self, key: &str, now: Instant) {
        let mut map = self.map();
        map.retain(|_, until| now < *until);
        if map.len() >= MAX_REMEMBERED {
            map.clear();
        }
        map.insert(key.to_string(), now + REMEMBER_OLD);
    }

    fn forget(&self, key: &str) {
        self.map().remove(key);
    }
}

static OLD: LazyLock<OldServers> = LazyLock::new(OldServers::new);

/// The summary address: the service's address with `/api/v1/summary` added
/// to its path. `http` or `https`, a host, no user name or password.
fn target(settings: &Json) -> Result<Url, FetchError> {
    let text = settings
        .get("url")
        .and_then(Json::as_str)
        .ok_or(FetchError::Config)?;
    let mut url = Url::parse(text.trim()).map_err(|_| FetchError::Config)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(FetchError::Config);
    }
    let host = url.host_str().ok_or(FetchError::Config)?;
    // An address in the URL never reaches the guarded resolver.
    check_host(host).map_err(|_| FetchError::Forbidden)?;
    url.set_query(None);
    url.set_fragment(None);
    url.path_segments_mut()
        .map_err(|_| FetchError::Config)?
        .pop_if_empty()
        .extend(["api", "v1", "summary"]);
    url.query_pairs_mut().append_pair("hours", HOURS);
    Ok(url)
}

/// The token as a header value that stays out of debug output.
fn bearer(secret: Option<&str>) -> Result<HeaderValue, FetchError> {
    let secret = secret.filter(|s| !s.is_empty()).ok_or(FetchError::Config)?;
    let mut value =
        HeaderValue::from_str(&format!("Bearer {secret}")).map_err(|_| FetchError::Config)?;
    value.set_sensitive(true);
    Ok(value)
}

/// A count in `totals`: a whole number, not below 0.
fn count(totals: &Map<String, Json>, key: &str) -> Result<f64, FetchError> {
    totals
        .get(key)
        .and_then(Json::as_u64)
        .map(|n| n as f64)
        .ok_or(FetchError::Invalid)
}

/// A number in `totals` that may be `null` or missing (no project has it, or
/// the server is a little older than this code).
fn optional(totals: &Map<String, Json>, key: &str) -> Result<Option<f64>, FetchError> {
    match totals.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Number(n)) => match n.as_f64() {
            Some(n) if n.is_finite() && n >= 0.0 => Ok(Some(n)),
            _ => Err(FetchError::Invalid),
        },
        Some(_) => Err(FetchError::Invalid),
    }
}

/// A title as one short line: control characters become spaces, and it is cut
/// at [`MAX_TITLE`] characters. `None` when nothing is left.
fn clean_title(title: &str) -> Option<String> {
    let line: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_TITLE)
        .collect();
    let line = line.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// The title of the issue that was first seen last, over all projects (the
/// later `first_seen`, then the higher id, so the order of the projects does
/// not matter). `first_seen` is `YYYY-MM-DD HH:MM:SS` in UTC, which sorts as
/// text.
fn newest_title(doc: &Json) -> Option<String> {
    let projects = doc.get("projects")?.as_array()?;
    projects
        .iter()
        .filter_map(|p| {
            let issue = p.get("newest_issue")?;
            let first_seen = issue.get("first_seen")?.as_str()?;
            let id = issue.get("id")?.as_i64()?;
            let title = issue.get("title")?.as_str()?;
            Some((first_seen, id, title))
        })
        .max_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)))
        .and_then(|(_, _, title)| clean_title(title))
}

/// A need with a count: the key and the label are fixed, and the score is the
/// count, so an acknowledgement is undone only when it grows. The page adds
/// the service's name, so there is no `name` param.
fn need(key: &str, level: Level, label_key: &str, count: f64) -> Need {
    Need {
        key: key.to_string(),
        level,
        label_key: label_key.to_string(),
        params: BTreeMap::from([("count".to_string(), format!("{count:.0}"))]),
        score: count,
    }
}

/// The snapshot a summary answer stands for.
fn read(body: &[u8]) -> Result<Snapshot, FetchError> {
    let doc: Json = serde_json::from_slice(body).map_err(|_| FetchError::Invalid)?;
    let totals = doc
        .get("totals")
        .and_then(Json::as_object)
        .ok_or(FetchError::Invalid)?;
    let new = count(totals, "new_issues")?;
    let regressed = count(totals, "regressed_issues")?;
    let unresolved = count(totals, "unresolved_issues")?;

    let mut values = BTreeMap::new();
    values.insert("new_issues".to_string(), Value::num(new));
    values.insert("regressed_issues".to_string(), Value::num(regressed));
    values.insert("unresolved_issues".to_string(), Value::num(unresolved));
    // A percentage from 0 to 100; a number above it is clamped.
    if let Some(pct) = optional(totals, "crash_free_sessions")? {
        values.insert("crash_free".to_string(), Value::num(pct.min(100.0)));
    }
    for key in ["visitors_today", "app_users_today"] {
        if let Some(n) = optional(totals, key)? {
            values.insert(key.to_string(), Value::num(n));
        }
    }
    if let Some(title) = newest_title(&doc) {
        values.insert("newest_issue".to_string(), Value::text(title));
    }

    let mut needs = Vec::new();
    if regressed > 0.0 {
        needs.push(need(
            "kk:regressed",
            Level::Bad,
            "need.kk_regressed",
            regressed,
        ));
    }
    if new > 0.0 {
        needs.push(need("kk:new", Level::Warn, "need.kk_new", new));
    }
    let status = if regressed > 0.0 {
        Status::Bad
    } else if new > 0.0 {
        Status::Warn
    } else {
        Status::Ok
    };

    let mut snapshot = Snapshot::new(status);
    snapshot.values = values;
    snapshot.needs = needs;
    Ok(snapshot)
}

impl Integration for KkKind {
    fn kind(&self) -> &'static str {
        "kk"
    }

    fn label_key(&self) -> &'static str {
        "kind.kk"
    }

    fn fields(&self) -> &'static [Field] {
        &FIELDS
    }

    fn metrics(&self) -> &'static [MetricDef] {
        &METRICS
    }

    fn default_interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn min_interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn fetch<'a>(&'a self, ctx: &'a FetchCtx) -> BoxFuture<'a, Result<Snapshot, FetchError>> {
        Box::pin(async move {
            let url = target(&ctx.settings)?;
            let token = bearer(ctx.secret.as_deref())?;
            let key = url.as_str().to_string();
            if OLD.is_old(&key, Instant::now()) {
                return Err(FetchError::KkTooOld);
            }
            let response = ctx
                .http
                .get(url)
                .header(AUTHORIZATION, token)
                .send()
                .await
                .map_err(|e| classify(&e))?;
            match response.status().as_u16() {
                401 | 403 => return Err(FetchError::Auth),
                404 => {
                    OLD.mark(&key, Instant::now());
                    return Err(FetchError::KkTooOld);
                }
                code if !(200..300).contains(&code) => return Err(FetchError::Status(code)),
                _ => {}
            }
            OLD.forget(&key);
            read(&read_capped(response).await?)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{MAX_REMEMBERED, OldServers, REMEMBER_OLD, clean_title};

    #[test]
    fn an_old_server_is_remembered_for_an_hour() {
        let old = OldServers::new();
        let start = Instant::now();
        assert!(!old.is_old("a", start));
        old.mark("a", start);
        assert!(old.is_old("a", start));
        assert!(old.is_old("a", start + REMEMBER_OLD - Duration::from_secs(1)));
        assert!(!old.is_old("a", start + REMEMBER_OLD));
        // Once the hour is over it is asked again, and is gone from memory.
        assert!(!old.is_old("a", start + REMEMBER_OLD + Duration::from_secs(1)));
        assert!(!old.is_old("b", start));
    }

    #[test]
    fn the_memory_is_bounded() {
        let old = OldServers::new();
        let start = Instant::now();
        for n in 0..1000 {
            old.mark(&format!("http://host-{n}/"), start);
        }
        assert!(old.map().len() <= MAX_REMEMBERED);
    }

    #[test]
    fn titles_are_trimmed_and_cut() {
        assert_eq!(clean_title("  a\nb  ").as_deref(), Some("a b"));
        assert_eq!(clean_title("\n\t "), None);
        assert_eq!(clean_title(&"é".repeat(500)).unwrap().chars().count(), 200);
    }
}
