//! The `http` kind («Nettside»): one GET to an address, and whether it
//! answered with an expected status and how fast. It is useful on its own for
//! a site that has no integration of its own, and it is the kind the engine is
//! tested with.
//!
//! The service's secret, when it has one, is sent as `Authorization: Bearer`,
//! and then a redirect to another origin is refused.
//!
//! **Details.** For a stored service, the shortest, mean and longest response
//! time of the last hour (the status, the response time and when it was
//! checked are metrics already). The hour is a small window kept in memory per
//! service (at most
//! [`WINDOW_MAX`] answers; forgotten when the service is saved or tested, see
//! [`api::on_forget`]) until history is kept in the database.

use std::collections::VecDeque;
use std::ops::RangeInclusive;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp};
use serde_json::Value as Json;
use url::Url;
use vole_kit::net::check_host;

use super::api;
use super::block::{Block, Fact};
use super::{
    BoxFuture, FetchCtx, FetchError, Field, FieldKind, Format, Integration, MetricDef,
    SettingError, Snapshot, Status, Value, classify, read_capped,
};

/// The only instance.
pub static HTTP: HttpKind = HttpKind;

/// See the module.
pub struct HttpKind;

/// The statuses that count as well when the service names none.
const DEFAULT_EXPECTED: RangeInclusive<u16> = 200..=399;

/// How far back the window of response times reaches.
const WINDOW_FOR: SignedDuration = SignedDuration::from_hours(1);
/// Most answers one service's window holds (a check every 15 s, the shortest
/// interval, makes 240 in an hour).
pub const WINDOW_MAX: usize = 300;
/// Most services whose window is kept; past that the one used longest ago goes.
pub const MEMORY_MAX: usize = 256;

/// A service as the window knows it: its address (see [`api::service_prefix`])
/// and its number.
type WindowKey = (String, i64);

/// The response times of the last hour, in milliseconds, per service.
static MEMORY: Mutex<VecDeque<(WindowKey, Samples)>> = Mutex::new(VecDeque::new());

/// When each answer came and how long it took, in milliseconds.
type Samples = VecDeque<(Timestamp, f64)>;

static FIELDS: [Field; 2] = [
    Field {
        key: "expected_status",
        label_key: "field.http_expected",
        help_key: "field.http_expected_help",
        placeholder: "200-399",
        kind: FieldKind::Text,
        required: false,
        secret: false,
    },
    Field {
        key: "secret",
        label_key: "field.http_secret",
        help_key: "field.http_secret_help",
        placeholder: "",
        kind: FieldKind::Text,
        required: false,
        secret: true,
    },
];

static METRICS: [MetricDef; 3] = [
    MetricDef {
        id: "status",
        label_key: "metric.http_status",
        format: Format::Number,
    },
    MetricDef {
        id: "latency_ms",
        label_key: "metric.http_latency",
        format: Format::Number,
    },
    MetricDef {
        id: "checked",
        label_key: "metric.http_checked",
        format: Format::Since,
    },
];

/// `200`, `200-299` or a comma-separated list of those. `None` when the text
/// is not that.
pub fn parse_expected(text: &str) -> Option<Vec<RangeInclusive<u16>>> {
    let mut ranges = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        let (low, high) = match part.split_once('-') {
            Some((low, high)) => (low.trim(), high.trim()),
            None => (part, part),
        };
        let (low, high): (u16, u16) = (low.parse().ok()?, high.parse().ok()?);
        if !(100..=599).contains(&low) || !(100..=599).contains(&high) || low > high {
            return None;
        }
        ranges.push(low..=high);
    }
    Some(ranges)
}

/// The shortest, mean and longest response time of the window, and how many
/// answers it holds.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Range {
    min: f64,
    mean: f64,
    max: f64,
    count: usize,
}

/// Takes `latency_ms` (when the site answered) at `now` into the window of
/// `key` and gives what the window holds. The window is cut to the last hour
/// and to [`WINDOW_MAX`] answers; the list of windows to [`MEMORY_MAX`], the
/// one used longest ago going first.
fn window(key: &WindowKey, now: Timestamp, latency_ms: Option<f64>) -> Option<Range> {
    // Saving or testing the service forgets its window.
    api::on_forget(forget);
    // A panic elsewhere cannot leave the list in a state that matters.
    let mut list = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    let mut entry = match list.iter().position(|(k, _)| k == key) {
        Some(at) => list.remove(at).expect("the position is in the list"),
        None => {
            while list.len() >= MEMORY_MAX {
                list.pop_front();
            }
            (key.clone(), VecDeque::new())
        }
    };
    let samples = &mut entry.1;
    if let Some(ms) = latency_ms.filter(|ms| ms.is_finite()) {
        samples.push_back((now, ms));
    }
    samples.retain(|(at, _)| now.duration_since(*at) <= WINDOW_FOR);
    while samples.len() > WINDOW_MAX {
        samples.pop_front();
    }
    let range = range(samples.iter().map(|(_, ms)| *ms));
    list.push_back(entry);
    range
}

fn range(times: impl Iterator<Item = f64>) -> Option<Range> {
    let (mut min, mut max, mut sum, mut count) = (f64::INFINITY, f64::NEG_INFINITY, 0.0, 0_usize);
    for ms in times {
        min = min.min(ms);
        max = max.max(ms);
        sum += ms;
        count += 1;
    }
    (count > 0).then(|| Range {
        min,
        mean: sum / count as f64,
        max,
        count,
    })
}

/// Forgets the window of the service at `prefix` (see [`api::service_prefix`]).
fn forget(prefix: &str) {
    MEMORY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|((key, _), _)| key != prefix);
}

fn fact(label_key: &str, value: String) -> Fact {
    Fact {
        label_key: Some(label_key.to_string()),
        label: None,
        value,
        ..Default::default()
    }
}

/// The block of the last hour's range: shortest, mean and longest response
/// time, and how many answers the window holds. Nothing until it holds at
/// least two answers (one says nothing a range could). The status, the
/// response time and when it was checked are the card's metrics, so no block
/// repeats them.
fn details(hour: Option<Range>) -> Vec<Block> {
    let Some(hour) = hour.filter(|r| r.count >= 2) else {
        return Vec::new();
    };
    vec![Block::Facts {
        title_key: "http_last_hour".to_string(),
        rows: vec![
            fact(
                "http_latency_range",
                format!("{:.0} / {:.0} / {:.0} ms", hour.min, hour.mean, hour.max),
            ),
            fact("http_samples", hour.count.to_string()),
        ],
    }]
}

/// `snapshot` with the `checked` metric: when the reading was taken, also when
/// the site did not answer.
fn checked(mut snapshot: Snapshot) -> Snapshot {
    snapshot.values.insert(
        "checked".to_string(),
        Value::num(snapshot.fetched_at.as_second() as f64),
    );
    snapshot
}

/// The address to fetch: `http` or `https`, a host, no user name or password.
fn target(settings: &Json) -> Result<Url, FetchError> {
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

fn expected(settings: &Json) -> Result<Vec<RangeInclusive<u16>>, FetchError> {
    match settings.get("expected_status").and_then(Json::as_str) {
        Some(text) if !text.trim().is_empty() => parse_expected(text).ok_or(FetchError::Config),
        _ => Ok(vec![DEFAULT_EXPECTED]),
    }
}

impl Integration for HttpKind {
    fn kind(&self) -> &'static str {
        "http"
    }

    fn label_key(&self) -> &'static str {
        "kind.http"
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
        Duration::from_secs(15)
    }

    fn check_settings(&self, settings: &serde_json::Map<String, Json>) -> Result<(), SettingError> {
        match settings.get("expected_status").and_then(Json::as_str) {
            Some(text) if !text.trim().is_empty() && parse_expected(text).is_none() => {
                Err(SettingError {
                    field: "expected_status",
                    message_key: "err.http_expected",
                })
            }
            _ => Ok(()),
        }
    }

    fn fetch<'a>(&'a self, ctx: &'a FetchCtx) -> BoxFuture<'a, Result<Snapshot, FetchError>> {
        Box::pin(async move {
            let url = target(&ctx.settings)?;
            let expected = expected(&ctx.settings)?;
            // The window is kept for a stored service only (a test of a form
            // has no number).
            let key: Option<WindowKey> = ctx.service_id().map(|id| (api::service_prefix(&url), id));
            let request = ctx.http.get(url);
            let started = Instant::now();
            let sent = match ctx.secret.as_deref().filter(|s| !s.is_empty()) {
                // With a secret, a redirect to another origin is refused.
                Some(secret) => {
                    let credential = api::Credential::bearer(Some(secret))?;
                    api::send_credentialed(credential.apply(request)).await
                }
                None => request.send().await.map_err(|err| classify(&err)),
            };
            let response = match sent {
                Ok(response) => response,
                // The site not answering is the reading, not a failure of ours,
                // and it was checked: «Last checked» says when.
                Err(err) => {
                    let snapshot = checked(Snapshot::failed(&err));
                    let hour = key
                        .as_ref()
                        .and_then(|key| window(key, snapshot.fetched_at, None));
                    return Ok(snapshot.with_details(details(hour)));
                }
            };
            let code = response.status().as_u16();
            let body = read_capped(response).await;
            let latency_ms = (started.elapsed().as_secs_f64() * 1000.0).round();

            let mut snapshot = checked(Snapshot::new(Status::Ok));
            snapshot
                .values
                .insert("status".to_string(), Value::num(f64::from(code)));
            snapshot
                .values
                .insert("latency_ms".to_string(), Value::num(latency_ms));
            if let Err(err) = body {
                snapshot.status = Status::Bad;
                snapshot.error = Some(err.to_string());
            } else if !expected.iter().any(|range| range.contains(&code)) {
                snapshot.status = Status::Bad;
                snapshot.error = Some(FetchError::Status(code).to_string());
            }
            let hour = key
                .as_ref()
                .and_then(|key| window(key, snapshot.fetched_at, Some(latency_ms)));
            Ok(snapshot.with_details(details(hour)))
        })
    }
}

#[cfg(test)]
mod tests {
    use jiff::{SignedDuration, Timestamp};

    use super::{MEMORY_MAX, WINDOW_MAX, parse_expected, window};

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + secs).unwrap()
    }

    #[test]
    fn the_window_holds_the_last_hour_and_is_bounded() {
        let key = ("http://window-test.example/".to_string(), 1);
        // Three answers: the range of them.
        for (secs, ms) in [(0, 100.0), (60, 300.0)] {
            window(&key, at(secs), Some(ms));
        }
        let r = window(&key, at(120), Some(200.0)).unwrap();
        assert_eq!((r.min, r.mean, r.max, r.count), (100.0, 200.0, 300.0, 3));
        // A check that got no answer adds nothing, and only reads.
        assert_eq!(window(&key, at(180), None).unwrap().count, 3);
        // An hour on, the first two are out.
        let hour = SignedDuration::from_hours(1).as_secs();
        let r = window(&key, at(hour + 90), Some(50.0)).unwrap();
        assert_eq!((r.min, r.max, r.count), (50.0, 200.0, 2));
        // A burst is cut to WINDOW_MAX answers.
        let key = ("http://window-test.example/burst/".to_string(), 2);
        let mut last = None;
        for i in 0..(WINDOW_MAX as i64 + 50) {
            last = window(&key, at(10_000 + i), Some(1.0));
        }
        assert_eq!(last.unwrap().count, WINDOW_MAX);
        // A number that is not finite is not kept.
        assert_eq!(
            window(&key, at(10_400), Some(f64::NAN)).unwrap().count,
            WINDOW_MAX
        );
        // And only so many services are remembered (in this test, since the
        // list is shared and eviction would reach the other keys).
        for n in 0..(MEMORY_MAX + 20) {
            window(
                &(format!("http://many-{n}.example/"), n as i64),
                at(0),
                Some(1.0),
            );
        }
        let held = super::MEMORY.lock().unwrap().len();
        assert!(held <= MEMORY_MAX, "{held}");
    }

    /// The ranges as pairs.
    fn pairs(text: &str) -> Option<Vec<(u16, u16)>> {
        parse_expected(text).map(|r| r.iter().map(|r| (*r.start(), *r.end())).collect())
    }

    #[test]
    fn expected_status_text() {
        assert_eq!(pairs("200"), Some(vec![(200, 200)]));
        assert_eq!(pairs(" 200 - 299 "), Some(vec![(200, 299)]));
        assert_eq!(pairs("200-299, 404"), Some(vec![(200, 299), (404, 404)]));
        for bad in [
            "", "abc", "99", "600", "300-200", "200-", "-200", "200,,300",
        ] {
            assert_eq!(pairs(bad), None, "{bad:?}");
        }
    }
}
