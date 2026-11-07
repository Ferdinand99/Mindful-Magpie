//! The `kk` kind («Keen Kestrel»): the error tracking and analytics app, read
//! through `GET /api/v1/summary?hours=24` with a read token.
//!
//! One call gives what the card shows: the issues that are new, regressed or
//! unresolved, crash-free sessions and today's visitors and app users, added
//! up over every project, plus each project's newest issue. The endpoint
//! shipped in Keen Kestrel 0.10.0; an older server answers 404, which this
//! kind turns into [`FetchError::KkTooOld`] and does not ask again for a few
//! minutes (see [`api::OLD_FOR`]; saving or testing the service asks again at
//! once).
//!
//! The token is sent as `Authorization: Bearer` and is marked sensitive, so
//! it stays out of debug output. Nothing here puts a token, an address or a
//! response text into an error. The only text taken from the answer is the
//! newest issue's title, and it goes into a `text` metric only: needs carry
//! a count. A field that is missing or odd costs only its own metric.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::Value as Json;

use super::api::{self, OldAddresses, Reply};
use super::{
    BoxFuture, FetchCtx, FetchError, Field, FieldKind, Format, Integration, Level, MetricDef, Need,
    Snapshot, Status, Value,
};

/// The only instance.
pub static KK: KkKind = KkKind;

/// See the module.
pub struct KkKind;

/// The window the summary is asked for.
const HOURS: &str = "24";
/// The longest newest-issue title kept, in characters.
const MAX_TITLE: usize = 200;

/// The addresses whose `/summary` answered 404.
static OLD: OldAddresses = OldAddresses::new();

static FIELDS: [Field; 1] = [Field {
    key: "secret",
    label_key: "field.kk_secret",
    help_key: "field.kk_secret_help",
    placeholder: "kk_…",
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

/// The title of the issue that was first seen last, over all projects (the
/// later `first_seen`, then the higher id, so the order of the projects does
/// not matter), cleaned as one short line. `first_seen` is
/// `YYYY-MM-DD HH:MM:SS` in UTC, which sorts as text.
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
        .map(|(_, _, title)| api::clean_text(title, MAX_TITLE))
        .filter(|title| !title.is_empty())
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
        since: None,
    }
}

/// The snapshot a summary answer stands for. Without `totals`, or with none
/// of its numbers usable, there is nothing to show: [`FetchError::Invalid`].
fn read_summary(doc: &Json) -> Result<Snapshot, FetchError> {
    let totals = doc
        .get("totals")
        .filter(|t| t.is_object())
        .ok_or(FetchError::Invalid)?;
    let mut values = BTreeMap::new();
    for (id, key) in [
        ("new_issues", "new_issues"),
        ("regressed_issues", "regressed_issues"),
        ("unresolved_issues", "unresolved_issues"),
        ("visitors_today", "visitors_today"),
        ("app_users_today", "app_users_today"),
    ] {
        if let Some(n) = api::count(totals.get(key)) {
            values.insert(id.to_string(), Value::num(n));
        }
    }
    if let Some(pct) = api::percent(totals.get("crash_free_sessions")) {
        values.insert("crash_free".to_string(), Value::num(pct));
    }
    if values.is_empty() {
        return Err(FetchError::Invalid);
    }
    if let Some(title) = newest_title(doc) {
        values.insert("newest_issue".to_string(), Value::text(title));
    }

    let number = |id: &str| values.get(id).and_then(|v| v.num).unwrap_or(0.0);
    let (new, regressed) = (number("new_issues"), number("regressed_issues"));
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

async fn read(ctx: &FetchCtx) -> Result<Snapshot, FetchError> {
    let base = api::base_url(&ctx.settings)?;
    let auth = api::Credential::bearer(ctx.secret.as_deref())?;
    let path = ["api", "v1", "summary"];
    let key = api::endpoint(&base, &path, None)?.to_string();
    if OLD.is_old(&key, Instant::now()) {
        return Err(FetchError::KkTooOld);
    }
    let url = api::endpoint(&base, &path, Some(&format!("hours={HOURS}")))?;
    match api::get_json::<Json>(&ctx.http, url, &auth).await? {
        Reply::Found(doc) => {
            OLD.forget(&key);
            read_summary(&doc)
        }
        Reply::NotFound => {
            OLD.mark(&key, Instant::now());
            Err(FetchError::KkTooOld)
        }
    }
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
        Box::pin(read(ctx))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn odd_numbers_drop_only_their_own_metric() {
        let doc = json!({ "totals": {
            "new_issues": 2,
            "regressed_issues": -4,
            "unresolved_issues": "many",
            "crash_free_sessions": 250.0,
            "visitors_today": null,
            "app_users_today": 1.5e300,
        } });
        let snapshot = read_summary(&doc).unwrap();
        let ids: Vec<_> = snapshot.values.keys().map(String::as_str).collect();
        assert_eq!(ids, ["app_users_today", "crash_free", "new_issues"]);
        assert_eq!(snapshot.values["crash_free"].num, Some(100.0));
        assert_eq!(snapshot.status, Status::Warn);
    }

    #[test]
    fn nothing_usable_is_not_a_reading() {
        for doc in [
            json!({}),
            json!({ "totals": null }),
            json!({ "totals": {} }),
            json!({ "totals": { "new_issues": "x" } }),
        ] {
            assert_eq!(read_summary(&doc).unwrap_err(), FetchError::Invalid);
        }
    }

    #[test]
    fn the_newest_title_is_cleaned() {
        let doc = json!({ "totals": { "new_issues": 0 }, "projects": [
            { "newest_issue": { "id": 1, "first_seen": "2026-01-01 00:00:00", "title": "old" } },
            { "newest_issue": { "id": 2, "first_seen": "2026-02-01 00:00:00",
                                "title": "  a\u{202E}\nb  " } },
        ] });
        assert_eq!(newest_title(&doc).as_deref(), Some("a b"));
    }
}
