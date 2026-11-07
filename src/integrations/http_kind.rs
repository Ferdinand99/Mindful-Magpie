//! The `http` kind («Nettside»): one GET to an address, and whether it
//! answered with an expected status and how fast. It is useful on its own for
//! a site that has no integration of its own, and it is the kind the engine is
//! tested with.
//!
//! The service's secret, when it has one, is sent as `Authorization: Bearer`.

use std::ops::RangeInclusive;
use std::time::{Duration, Instant};

use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde_json::Value as Json;
use url::Url;
use vole_kit::net::check_host;

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
            let mut request = ctx.http.get(url);
            if let Some(secret) = ctx.secret.as_deref().filter(|s| !s.is_empty()) {
                let mut value = HeaderValue::from_str(&format!("Bearer {secret}"))
                    .map_err(|_| FetchError::Config)?;
                // Kept out of debug output, and stripped on a redirect to
                // another host.
                value.set_sensitive(true);
                request = request.header(AUTHORIZATION, value);
            }
            let started = Instant::now();
            let response = match request.send().await {
                Ok(response) => response,
                // The site not answering is the reading, not a failure of ours,
                // and it was checked: «Last checked» says when.
                Err(err) => return Ok(checked(Snapshot::failed(&classify(&err)))),
            };
            let code = response.status().as_u16();
            let body = read_capped(response).await;
            let latency_ms = started.elapsed().as_secs_f64() * 1000.0;

            let mut snapshot = checked(Snapshot::new(Status::Ok));
            snapshot
                .values
                .insert("status".to_string(), Value::num(f64::from(code)));
            snapshot
                .values
                .insert("latency_ms".to_string(), Value::num(latency_ms.round()));
            if let Err(err) = body {
                snapshot.status = Status::Bad;
                snapshot.error = Some(err.to_string());
            } else if !expected.iter().any(|range| range.contains(&code)) {
                snapshot.status = Status::Bad;
                snapshot.error = Some(FetchError::Status(code).to_string());
            }
            Ok(snapshot)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::parse_expected;

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
