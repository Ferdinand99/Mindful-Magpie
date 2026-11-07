//! The `ww` kind («Wise Wren»): the servers, updates and update runs of a
//! Wise Wren instance, read through its API with a read token.
//!
//! Three calls a fetch, made together and each bounded: `GET /api/v1/servers`
//! (the agents and whether they are online), `GET /api/v1/updates` (the
//! targets with a new version) and `GET /api/v1/runs?limit=100` (one page of
//! the newest runs, never followed on). A call that fails costs only its own
//! numbers: the reading turns bad, says why, and keeps what the other calls
//! gave (when none of them gave anything the fetch is an `Err`). Wise Wren's
//! own names (servers, targets, images, versions) go only into the detail
//! blocks, which `Block::bounded` cuts down; the metrics and needs hold
//! numbers only.
//!
//! **Details.** «Ready updates» lists the targets with a new version (name,
//! the versions or release title or image below it, the server at the right),
//! «Last run» is the newest run (its result as the label, target, server and
//! age as the value) with how many updates went through and failed in the last
//! 24 hours, and «Agents» lists the servers with a status dot (online,
//! offline, anything else unknown).
//!
//! The token is sent as `Authorization: Bearer`, marked sensitive so it stays
//! out of debug output.

use std::collections::HashSet;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

use super::api;
use super::block::{Block, Fact, ListItem, MAX_ITEMS, TEXT_CHARS};
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
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    last_seen_at: Option<String>,
}

/// A server as another record names it.
#[derive(Deserialize)]
struct ServerRef {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct TargetList {
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Target {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    server: Option<ServerRef>,
    #[serde(default)]
    installed_version: Option<String>,
    #[serde(default)]
    latest_version: Option<String>,
    #[serde(default)]
    release_notes: Option<ReleaseNotes>,
}

#[derive(Deserialize)]
struct ReleaseNotes {
    #[serde(default)]
    title: Option<String>,
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
    server: Option<ServerRef>,
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
    #[serde(default)]
    name: Option<String>,
}

impl Run {
    /// When the run happened: its end, else its start, else its creation.
    fn time(&self) -> Option<Timestamp> {
        [&self.finished_at, &self.started_at, &self.created_at]
            .into_iter()
            .flatten()
            .find_map(|text| api::parse_time(text))
    }
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
    let (mut ready_block, mut run_block, mut agents_block) = (None, None, None);

    match servers {
        Ok(servers) => {
            let count = |status: &str| {
                servers
                    .iter()
                    .filter(|s| s.status.as_deref() == Some(status))
                    .count()
            };
            offline = count("offline");
            agents_block = agents_list(&servers, now);
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
            ready_block = ready_list(&targets);
        }
        Err(err) => error = Some(worse(error, err)),
    }
    match runs {
        Ok(runs) => {
            failed = failed_targets(&runs, now);
            snapshot
                .values
                .insert("failed_targets".into(), Value::num(failed as f64));
            run_block = last_run_facts(&runs, now);
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
    snapshot.details = [ready_block, run_block, agents_block]
        .into_iter()
        .flatten()
        .collect();
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
    // A call that failed leaves the reading partial, which is a warning (as for
    // Unraid and Tofa); only a failed update is bad. Updates ready are shown,
    // not a warning: whether they matter is up to the user (Wise Wren's list
    // does not say which ones were skipped).
    if let Some(err) = error {
        snapshot.error = Some(err.to_string());
    }
    snapshot.status = if failed > 0 {
        Status::Bad
    } else if offline > 0 || snapshot.error.is_some() {
        Status::Warn
    } else {
        Status::Ok
    };
    Ok(snapshot)
}

/// A name or text from Wise Wren for a block (cut down again by `bounded`).
fn text(text: Option<&str>) -> Option<String> {
    text.map(|t| api::clean_text(t, TEXT_CHARS))
        .filter(|t| !t.is_empty())
}

/// The list of targets with a new version: the name, `installed → latest`
/// when Wise Wren knows both versions (else the release's title, else the
/// image), and the server.
fn ready_list(targets: &[Target]) -> Option<Block> {
    let items: Vec<ListItem> = targets
        .iter()
        .filter(|t| t.status.as_deref().is_none_or(|s| s == "update"))
        .filter_map(|t| {
            let versions = match (
                text(t.installed_version.as_deref()),
                text(t.latest_version.as_deref()),
            ) {
                (Some(from), Some(to)) => Some(format!("{from} → {to}")),
                _ => None,
            };
            Some(ListItem {
                text: text(t.name.as_deref())?,
                sub: versions
                    .or_else(|| text(t.release_notes.as_ref()?.title.as_deref()))
                    .or_else(|| text(t.image.as_deref())),
                status: None,
                right: t.server.as_ref().and_then(|s| text(s.name.as_deref())),
                href: None,
            })
        })
        .take(MAX_ITEMS)
        .collect();
    (!items.is_empty()).then(|| Block::List {
        title_key: "ww_ready_updates".to_string(),
        items,
    })
}

/// The newest run (the highest id) as a fact whose label is its result, and
/// how many updates went through and failed in the last 24 hours. `None`
/// without a run.
fn last_run_facts(runs: &[Run], now: Timestamp) -> Option<Block> {
    let newest = runs.iter().max_by_key(|run| run.id)?;
    let status = newest.status.as_deref().unwrap_or_default();
    let label_key = match status {
        "queued" | "running" | "succeeded" | "rolled_back" | "failed" | "rollback_failed"
        | "refused" | "cancelled" | "lapsed" => format!("ww_run_{status}"),
        _ => "ww_run_other".to_string(),
    };
    let value = [
        text(newest.target.as_ref().and_then(|t| t.name.as_deref())),
        text(newest.server.as_ref().and_then(|s| s.name.as_deref())),
        newest.time().map(|at| api::age_text(at, now)),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let mut rows = vec![Fact {
        label_key: Some(label_key),
        label: None,
        value: if value.is_empty() {
            "-".to_string()
        } else {
            value
        },
    }];
    let (mut updated, mut failed) = (0, 0);
    for run in runs {
        let status = run.status.as_deref().unwrap_or_default();
        let recent = run
            .time()
            .is_some_and(|at| now.duration_since(at) <= SignedDuration::from_hours(24));
        if recent && status == "succeeded" {
            updated += 1;
        } else if recent && FAILED.contains(&status) {
            failed += 1;
        }
    }
    for (count, label_key) in [
        (updated, "ww_runs_updated_24h"),
        (failed, "ww_runs_failed_24h"),
    ] {
        if count > 0 {
            rows.push(Fact {
                label_key: Some(label_key.to_string()),
                label: None,
                value: count.to_string(),
            });
        }
    }
    Some(Block::Facts {
        title_key: "ww_last_run".to_string(),
        rows,
    })
}

/// The agents, one line each with a status dot: online is ok, offline is bad,
/// waiting and without a token are unknown. An agent that is not online says
/// how long ago it was last seen.
fn agents_list(servers: &[Server], now: Timestamp) -> Option<Block> {
    let items: Vec<ListItem> = servers
        .iter()
        .filter_map(|s| {
            let status = match s.status.as_deref() {
                Some("online") => Status::Ok,
                Some("offline") => Status::Bad,
                _ => Status::Unknown,
            };
            let seen = s.last_seen_at.as_deref().and_then(api::parse_time);
            Some(ListItem {
                text: text(s.name.as_deref())?,
                sub: None,
                status: Some(status),
                right: seen
                    .filter(|_| status != Status::Ok)
                    .map(|at| api::age_text(at, now)),
                href: None,
            })
        })
        .take(MAX_ITEMS)
        .collect();
    (!items.is_empty()).then(|| Block::List {
        title_key: "ww_agents".to_string(),
        items,
    })
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
