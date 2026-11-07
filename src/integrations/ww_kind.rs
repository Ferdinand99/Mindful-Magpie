//! The `ww` kind («Wise Wren»): the servers, updates and update runs of a
//! Wise Wren instance, read through its API with a read token.
//!
//! Three calls a fetch, made together and each bounded: `GET /api/v1/servers`
//! (the agents and whether they are online), `GET /api/v1/updates` (the
//! targets with a new version) and `GET /api/v1/runs?limit=100` (one page of
//! the newest runs, never followed on). A call that fails costs only its own
//! numbers: the reading turns bad, says why, and keeps what the other calls
//! gave (when none of them gave anything the fetch is an `Err`). Wise Wren's
//! own names (servers, targets, images) are never kept: the
//! reading holds numbers only.
//!
//! The token is sent as `Authorization: Bearer`, marked sensitive so it stays
//! out of debug output.

use std::collections::HashSet;
use std::time::Duration;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, civil};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

use super::api;
use super::{
    BoxFuture, FetchCtx, FetchError, Field, FieldKind, Format, Integration, Level, MetricDef, Need,
    Snapshot, Status, Value,
};

/// How many of the newest runs one fetch asks for. Wise Wren answers newest
/// first and has no cursor; this is enough to see the latest run of every
/// target that has been updated lately.
const RUN_LIMIT: u32 = 100;

/// A failed update older than this no longer needs anyone.
const FAILED_WINDOW: SignedDuration = SignedDuration::from_hours(3 * 24);

/// Run statuses that mean an update did not go through.
const FAILED: [&str; 3] = ["failed", "rolled_back", "rollback_failed"];

/// Run statuses that never ran, so they say nothing about whether the update
/// works: they neither make a failure nor hide one.
const NEVER_RAN: [&str; 3] = ["cancelled", "lapsed", "refused"];

/// The only instance.
pub static WW: WwKind = WwKind;

/// See the module.
pub struct WwKind;

static FIELDS: [Field; 1] = [Field {
    key: "secret",
    label_key: "field.ww_secret",
    help_key: "field.ww_secret_help",
    placeholder: "ww_…",
    kind: FieldKind::Text,
    required: true,
    secret: true,
}];

static METRICS: [MetricDef; 5] = [
    MetricDef {
        id: "updates_ready",
        label_key: "metric.ww_updates_ready",
        format: Format::Number,
    },
    MetricDef {
        id: "agents_online",
        label_key: "metric.ww_agents_online",
        format: Format::Number,
    },
    MetricDef {
        id: "agents_total",
        label_key: "metric.ww_agents_total",
        format: Format::Number,
    },
    MetricDef {
        id: "last_run",
        label_key: "metric.ww_last_run",
        format: Format::Since,
    },
    MetricDef {
        id: "failed_targets",
        label_key: "metric.ww_failed_targets",
        format: Format::Number,
    },
];

impl Integration for WwKind {
    fn kind(&self) -> &'static str {
        "ww"
    }

    fn label_key(&self) -> &'static str {
        "kind.ww"
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
            let base = api::base_url(&ctx.settings)?;
            let auth = api::Credential::bearer(ctx.secret.as_deref())?;

            let runs_query = format!("limit={RUN_LIMIT}");
            let (servers, updates, runs) = tokio::join!(
                call::<ServerList>(ctx, &base, "servers", None, &auth),
                call::<TargetList>(ctx, &base, "updates", None, &auth),
                call::<RunList>(ctx, &base, "runs", Some(&runs_query), &auth),
            );
            reading(
                servers.map(|l| l.servers),
                updates.map(|l| l.targets),
                runs.map(|l| l.runs),
                Timestamp::now(),
            )
        })
    }
}

#[derive(Deserialize)]
struct ServerList {
    servers: Vec<Server>,
}

#[derive(Deserialize)]
struct Server {
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize)]
struct TargetList {
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Target {
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize)]
struct RunList {
    runs: Vec<Run>,
}

#[derive(Deserialize)]
struct Run {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    target: Option<RunTarget>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    finished_at: Option<String>,
}

#[derive(Deserialize)]
struct RunTarget {
    #[serde(default)]
    id: Option<i64>,
}

impl Run {
    /// When the run happened: its end, else its start, else its creation.
    fn time(&self) -> Option<Timestamp> {
        [&self.finished_at, &self.started_at, &self.created_at]
            .into_iter()
            .flatten()
            .find_map(|text| parse_time(text))
    }
}

/// A time as Wise Wren writes it (`2026-10-23 10:00:00`, UTC) or as RFC 3339.
fn parse_time(text: &str) -> Option<Timestamp> {
    let text = text.trim();
    if let Ok(at) = text.parse::<Timestamp>() {
        return Some(at);
    }
    let at: civil::DateTime = text.parse().ok()?;
    Some(at.to_zoned(TimeZone::UTC).ok()?.timestamp())
}

/// One call to `/api/v1/<last>`. A 404 is an error too: there is nothing to
/// fall back on.
async fn call<T: DeserializeOwned>(
    ctx: &FetchCtx,
    base: &Url,
    last: &str,
    query: Option<&str>,
    auth: &api::Credential,
) -> Result<T, FetchError> {
    let url = api::endpoint(base, &["api", "v1", last], query)?;
    api::get_json(&ctx.http, url, auth).await?.found()
}

/// Which of two failures to report: a refused key first, since it is what an
/// admin can act on, else the first.
fn worse(old: Option<FetchError>, new: FetchError) -> FetchError {
    match old {
        Some(FetchError::Auth) => FetchError::Auth,
        Some(_) if new == FetchError::Auth => FetchError::Auth,
        Some(old) => old,
        None => new,
    }
}

/// The number of targets whose latest run failed. The runs may come in any
/// order (the newest is the one with the highest id); a run that never ran
/// (see [`NEVER_RAN`]) is passed over; a failure older than [`FAILED_WINDOW`]
/// is left alone.
fn failed_targets(runs: &[Run], now: Timestamp) -> usize {
    let mut newest_first: Vec<&Run> = runs.iter().collect();
    newest_first.sort_by_key(|run| std::cmp::Reverse(run.id));
    let mut seen = HashSet::new();
    let mut failed = 0;
    for run in newest_first {
        let Some(target) = run.target.as_ref().and_then(|t| t.id) else {
            continue;
        };
        let status = run.status.as_deref().unwrap_or_default();
        if NEVER_RAN.contains(&status) || !seen.insert(target) {
            continue;
        }
        let recent = run
            .time()
            .is_none_or(|at| now.duration_since(at) <= FAILED_WINDOW);
        if FAILED.contains(&status) && recent {
            failed += 1;
        }
    }
    failed
}

/// The reading from the three calls' results.
fn reading(
    servers: Result<Vec<Server>, FetchError>,
    updates: Result<Vec<Target>, FetchError>,
    runs: Result<Vec<Run>, FetchError>,
    now: Timestamp,
) -> Result<Snapshot, FetchError> {
    let mut snapshot = Snapshot::new(Status::Ok);
    let mut error = None;
    let (mut offline, mut failed) = (0, 0);

    match servers {
        Ok(servers) => {
            let count = |status: &str| {
                servers
                    .iter()
                    .filter(|s| s.status.as_deref() == Some(status))
                    .count()
            };
            offline = count("offline");
            snapshot
                .values
                .insert("agents_online".into(), Value::num(count("online") as f64));
            snapshot
                .values
                .insert("agents_total".into(), Value::num(servers.len() as f64));
        }
        Err(err) => error = Some(worse(error, err)),
    }
    match updates {
        Ok(targets) => {
            // `/updates` lists the targets with a new version.
            let ready = targets
                .iter()
                .filter(|t| t.status.as_deref().is_none_or(|s| s == "update"))
                .count();
            snapshot
                .values
                .insert("updates_ready".into(), Value::num(ready as f64));
        }
        Err(err) => error = Some(worse(error, err)),
    }
    match runs {
        Ok(runs) => {
            failed = failed_targets(&runs, now);
            snapshot
                .values
                .insert("failed_targets".into(), Value::num(failed as f64));
            let newest = runs.iter().max_by_key(|run| run.id);
            if let Some(at) = newest.and_then(Run::time) {
                snapshot
                    .values
                    .insert("last_run".into(), Value::num(at.as_second() as f64));
            }
        }
        Err(err) => error = Some(worse(error, err)),
    }

    if snapshot.values.is_empty()
        && let Some(err) = error
    {
        // Nothing could be read at all.
        return Err(err);
    }
    if failed > 0 {
        snapshot
            .needs
            .push(need("ww:failed", Level::Bad, "need.ww_failed", failed));
    }
    if offline > 0 {
        snapshot
            .needs
            .push(need("ww:offline", Level::Warn, "need.ww_offline", offline));
    }
    if let Some(err) = error {
        snapshot.status = Status::Bad;
        snapshot.error = Some(err.to_string());
    } else if failed > 0 {
        snapshot.status = Status::Bad;
    } else if offline > 0 {
        // Updates ready are shown, not a warning: whether they matter is up
        // to the user (Wise Wren's list does not say which ones were skipped).
        snapshot.status = Status::Warn;
    }
    Ok(snapshot)
}

/// A need whose only parameter is how many: the chip shows the service's name
/// itself, and nothing Wise Wren reported goes in.
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
