//! The `vv` kind («Vigilant Vole»): the uptime monitor. One call to
//! `GET /api/v1/summary?days=30` gives the monitor counts, the mean uptime
//! and what is down.
//!
//! An older Vigilant Vole (before 0.17.0) has no `/summary`: it answers 404.
//! The kind then asks `GET /api/v1/health`, which has the counts but no uptime
//! figures, and remembers for an hour that this address is old, so it does not
//! ask `/summary` again at every poll.
//!
//! The service's secret is a read token, sent as `Authorization: Bearer`.
//! Monitor names come from outside: they are shown only as the text value
//! `first_down`, never in a need.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue};
use serde::Deserialize;
use serde_json::Value as Json;
use url::Url;
use vole_kit::net::check_host;

use super::{
    BoxFuture, FetchCtx, FetchError, Field, FieldKind, Format, Integration, Level, MetricDef, Need,
    Snapshot, Status, Value, classify, read_capped,
};

/// The only instance.
pub static VV: VvKind = VvKind;

/// See the module.
pub struct VvKind;

/// The number of days the uptime figure covers.
const DAYS: &str = "30";
/// How long an address that answered 404 to `/summary` is left alone.
const LEGACY_FOR: Duration = Duration::from_secs(60 * 60);
/// Most addresses remembered as old.
const LEGACY_MAX: usize = 64;
/// Longest `first_down` text, in characters.
const NAME_CHARS: usize = 80;

/// The key of the need «some monitors are down».
pub const DOWN_NEED: &str = "vv:down";

static FIELDS: [Field; 1] = [Field {
    key: "token",
    label_key: "field.vv_token",
    help_key: "field.vv_token_help",
    placeholder: "",
    kind: FieldKind::Text,
    required: true,
    secret: true,
}];

static METRICS: [MetricDef; 6] = [
    MetricDef {
        id: "up",
        label_key: "metric.vv_up",
        format: Format::Number,
    },
    MetricDef {
        id: "down",
        label_key: "metric.vv_down",
        format: Format::Number,
    },
    MetricDef {
        id: "uptime_30d",
        label_key: "metric.vv_uptime_30d",
        format: Format::Percent,
    },
    MetricDef {
        id: "uptime_24h",
        label_key: "metric.vv_uptime_24h",
        format: Format::Percent,
    },
    MetricDef {
        id: "paused",
        label_key: "metric.vv_paused",
        format: Format::Number,
    },
    MetricDef {
        id: "first_down",
        label_key: "metric.vv_first_down",
        format: Format::Text,
    },
];

// ---------------------------------------------------------------------------
// What Vigilant Vole answers. Only what is read; every other field is ignored.

#[derive(Debug, Deserialize)]
struct Summary {
    monitors: Counts,
    #[serde(default)]
    uptime: Uptime,
    #[serde(default)]
    down: Vec<DownMonitor>,
}

#[derive(Debug, Default, Deserialize)]
struct Counts {
    #[serde(default)]
    up: u64,
    #[serde(default)]
    down: u64,
    #[serde(default)]
    pending: u64,
    #[serde(default)]
    unknown: u64,
    #[serde(default)]
    paused: u64,
}

#[derive(Debug, Default, Deserialize)]
struct Uptime {
    pct: Option<f64>,
    pct_24h: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct DownMonitor {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Health {
    monitors: HealthMonitors,
}

#[derive(Debug, Deserialize)]
struct HealthMonitors {
    #[serde(default)]
    by_status: BTreeMap<String, u64>,
}

impl From<HealthMonitors> for Counts {
    fn from(h: HealthMonitors) -> Counts {
        let count = |key: &str| h.by_status.get(key).copied().unwrap_or(0);
        Counts {
            up: count("up"),
            down: count("down"),
            pending: count("pending"),
            unknown: count("unknown"),
            paused: count("paused"),
        }
    }
}

// ---------------------------------------------------------------------------
// Old servers

/// The addresses that answered 404 to `/summary`, and when.
#[derive(Debug, Default)]
struct Legacy {
    seen: Vec<(String, Instant)>,
}

impl Legacy {
    fn expire(&mut self, now: Instant) {
        self.seen
            .retain(|(_, at)| now.saturating_duration_since(*at) < LEGACY_FOR);
    }

    /// Whether `key` answered 404 within the last hour.
    fn is_old(&mut self, key: &str, now: Instant) -> bool {
        self.expire(now);
        self.seen.iter().any(|(k, _)| k == key)
    }

    fn mark(&mut self, key: &str, now: Instant) {
        self.expire(now);
        self.seen.retain(|(k, _)| k != key);
        if self.seen.len() >= LEGACY_MAX {
            self.seen.remove(0);
        }
        self.seen.push((key.to_string(), now));
    }
}

static LEGACY: Mutex<Legacy> = Mutex::new(Legacy { seen: Vec::new() });

fn legacy<T>(f: impl FnOnce(&mut Legacy) -> T) -> T {
    // A panic elsewhere cannot leave the list in a state that matters.
    let mut guard = LEGACY.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

// ---------------------------------------------------------------------------
// Addresses and requests

/// The service's address: `http` or `https`, a host, no user name or password.
fn base(settings: &Json) -> Result<Url, FetchError> {
    let text = settings
        .get("url")
        .and_then(Json::as_str)
        .ok_or(FetchError::Config)?;
    let url = Url::parse(text.trim()).map_err(|_| FetchError::Config)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(FetchError::Config);
    }
    let host = url.host_str().ok_or(FetchError::Config)?;
    // An address in the URL never reaches the guarded resolver.
    check_host(host).map_err(|_| FetchError::Forbidden)?;
    Ok(url)
}

/// `base` with `/api/v1/<last>` after its own path (Vigilant Vole may sit
/// under a prefix behind a proxy).
fn endpoint(base: &Url, last: &str) -> Result<Url, FetchError> {
    let mut url = base.clone();
    url.set_query(None);
    url.set_fragment(None);
    url.path_segments_mut()
        .map_err(|()| FetchError::Config)?
        .pop_if_empty()
        .extend(["api", "v1", last]);
    Ok(url)
}

enum Reply {
    Body(Vec<u8>),
    NotFound,
}

/// One GET. The error is why there is no reading; it holds neither the
/// address nor the header.
async fn get(http: &reqwest::Client, url: Url, auth: &HeaderValue) -> Result<Reply, FetchError> {
    let response = http
        .get(url)
        .header(AUTHORIZATION, auth.clone())
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| classify(&e))?;
    match response.status().as_u16() {
        401 | 403 => Err(FetchError::Auth),
        404 => Ok(Reply::NotFound),
        code if !(200..300).contains(&code) => Err(FetchError::Status(code)),
        _ => read_capped(response).await.map(Reply::Body),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, FetchError> {
    serde_json::from_slice(body).map_err(|_| FetchError::Invalid)
}

// ---------------------------------------------------------------------------
// The reading

/// The text of a monitor's name that is safe to show: no control characters,
/// not long.
fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(NAME_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

fn percent(p: Option<f64>) -> Value {
    Value {
        num: p.filter(|p| p.is_finite()),
        text: None,
    }
}

fn snapshot(counts: &Counts, uptime: &Uptime, first_down: Option<String>) -> Snapshot {
    let status = if counts.down > 0 {
        Status::Bad
    } else if counts.pending + counts.unknown > 0 {
        Status::Warn
    } else {
        Status::Ok
    };
    let mut snapshot = Snapshot::new(status);
    let count = |n: u64| Value::num(n as f64);
    snapshot.values = BTreeMap::from([
        ("up".to_string(), count(counts.up)),
        ("down".to_string(), count(counts.down)),
        ("paused".to_string(), count(counts.paused)),
        ("uptime_30d".to_string(), percent(uptime.pct)),
        ("uptime_24h".to_string(), percent(uptime.pct_24h)),
        (
            "first_down".to_string(),
            match first_down.filter(|n| !n.is_empty()) {
                Some(name) => Value::text(name),
                None => Value {
                    num: None,
                    text: None,
                },
            },
        ),
    ]);
    if counts.down > 0 {
        snapshot.needs.push(down_need(counts.down));
    }
    snapshot
}

/// The need «n monitors are down». Its text takes only the count (the card
/// shows the service's name next to it); no monitor name goes in.
fn down_need(down: u64) -> Need {
    Need {
        key: DOWN_NEED.to_string(),
        level: Level::Bad,
        label_key: "need.vv_down".to_string(),
        params: BTreeMap::from([("count".to_string(), down.to_string())]),
        score: down as f64,
    }
}

async fn read(ctx: &FetchCtx, base: &Url, auth: &HeaderValue) -> Result<Snapshot, FetchError> {
    let mut summary = endpoint(base, "summary")?;
    let key = summary.as_str().to_string();
    if !legacy(|l| l.is_old(&key, Instant::now())) {
        summary.set_query(Some(&format!("days={DAYS}")));
        match get(&ctx.http, summary, auth).await? {
            Reply::Body(body) => {
                let s: Summary = parse(&body)?;
                let first = s.down.iter().find_map(|d| d.name.as_deref());
                return Ok(snapshot(&s.monitors, &s.uptime, first.map(clean_name)));
            }
            Reply::NotFound => legacy(|l| l.mark(&key, Instant::now())),
        }
    }
    // An old server: the counts of `/health`, and no uptime figures.
    match get(&ctx.http, endpoint(base, "health")?, auth).await? {
        Reply::Body(body) => {
            let h: Health = parse(&body)?;
            Ok(snapshot(&h.monitors.into(), &Uptime::default(), None))
        }
        // Not Vigilant Vole, or older than any this kind knows.
        Reply::NotFound => Err(FetchError::Status(404)),
    }
}

impl Integration for VvKind {
    fn kind(&self) -> &'static str {
        "vv"
    }

    fn label_key(&self) -> &'static str {
        "kind.vv"
    }

    fn fields(&self) -> &'static [Field] {
        &FIELDS
    }

    fn metrics(&self) -> &'static [MetricDef] {
        &METRICS
    }

    fn default_interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn min_interval(&self) -> Duration {
        Duration::from_secs(15)
    }

    fn fetch<'a>(&'a self, ctx: &'a FetchCtx) -> BoxFuture<'a, Result<Snapshot, FetchError>> {
        Box::pin(async move {
            // What the admin set wrong is an `Err`; what the server does is a
            // bad reading.
            let base = base(&ctx.settings)?;
            let token = ctx
                .secret
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or(FetchError::Config)?;
            let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| FetchError::Config)?;
            // Kept out of debug output, and stripped on a redirect to another
            // host.
            auth.set_sensitive(true);
            Ok(read(ctx, &base, &auth)
                .await
                .unwrap_or_else(|e| Snapshot::failed(&e)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_address_is_left_alone_for_an_hour() {
        let start = Instant::now();
        let mut l = Legacy::default();
        assert!(!l.is_old("a", start));
        l.mark("a", start);
        assert!(l.is_old("a", start + Duration::from_secs(59 * 60)));
        assert!(!l.is_old("b", start));
        assert!(!l.is_old("a", start + LEGACY_FOR));
        assert!(l.seen.is_empty());
    }

    #[test]
    fn the_list_of_old_addresses_stays_small() {
        let now = Instant::now();
        let mut l = Legacy::default();
        for i in 0..(LEGACY_MAX + 10) {
            l.mark(&format!("http://h{i}/"), now);
        }
        assert_eq!(l.seen.len(), LEGACY_MAX);
        assert!(!l.is_old("http://h0/", now));
        assert!(l.is_old(&format!("http://h{}/", LEGACY_MAX + 9), now));
    }

    #[test]
    fn the_endpoint_follows_the_path_of_the_address() {
        let at = |text: &str, last: &str| {
            endpoint(&Url::parse(text).unwrap(), last)
                .unwrap()
                .to_string()
        };
        assert_eq!(
            at("http://h:3000", "summary"),
            "http://h:3000/api/v1/summary"
        );
        assert_eq!(
            at("https://h/vole/", "health"),
            "https://h/vole/api/v1/health"
        );
        assert_eq!(
            at("https://h/vole?x=1#f", "health"),
            "https://h/vole/api/v1/health"
        );
    }

    #[test]
    fn a_name_is_cleaned_and_cut() {
        assert_eq!(clean_name("a\u{0}b\nc\u{7}"), "abc");
        assert_eq!(clean_name(&"x".repeat(500)).chars().count(), NAME_CHARS);
    }
}
