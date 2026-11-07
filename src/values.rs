//! What the start page and the extension know of a service's latest reading:
//! the [`ValuesDoc`] (embedded as `#mm-values`, served by
//! `GET /api/v1/me/values`) and the event stream `GET /start/live` that keeps
//! a page's copy of it current.
//!
//! **What goes in.** Only services that are bound on the user's own layout
//! *and* visible to them (the layout's bindings intersected with
//! [`store::visible_to`], the list [`crate::services::services_for`] is built
//! on). A binding the user can no longer see, or one to a service that is gone,
//! produces nothing: not even a name. The save keeps old bindings without
//! checking them again, so this filter is what keeps them quiet.
//!
//! **An entry** is `{ name, kind, interval_s, status, values, details?,
//! fetched_at, error_label? }`. `details` is the reading's blocks (lists, bars,
//! strips and facts; see [`crate::integrations::block`]), absent when there
//! are none: it follows the entry, so it is as visible as the values are, on the
//! page, in the stream and in `GET /api/v1/me/values`, and a client that does not
//! know the field ignores it. A visible bound service that has no snapshot yet is there
//! with `status: "unknown"`, no values and `fetched_at: null` (the renderer
//! shows "waiting"); an entry is therefore
//! absent only when the user must not know of the service. `error_label` is the
//! reason of a failed reading in the request's language, taken from the closed
//! set of texts (see [`error_label`]); the raw error never goes out. Times are
//! Unix seconds.
//!
//! **History.** An entry also carries the service's own history of its
//! numbers (see [`crate::history`]), for the metrics bound on the user's cards
//! for that service (the `metrics` of every card bound to it) and no others:
//!
//! * `history`: `{ "<metric>": [[unix_s, value], …] }`, the last 24 hours,
//!   oldest first, at most 288 points per metric, values rounded to 4
//!   significant digits (a whole number has no decimal point);
//! * `history_daily`: `{ "<metric>": [["YYYY-MM-DD", min, max, avg], …] }`, the
//!   last 30 UTC days, oldest first. It is sent for every bound metric, not
//!   only where a card asks for it: 30 short rows per metric (about 1 KiB) cost
//!   less than a second round trip to ask for them;
//! * `history_append`: `{ "<metric>": [unix_s, value] }`, in stream events only
//!   (below).
//!
//! A metric with no point yet (a service that was never read, a text) is left
//! out; every one of the three fields is left out when empty. The embed and
//! `GET /api/v1/me/values` carry `history` and `history_daily`, never
//! `history_append`.
//!
//! **Needs** («Trenger deg») are the document's own list, `needs`, one
//! [`NeedView`] per need of the services above, with the user's
//! acknowledgements applied (see [`crate::needs`]): the `config` need of a
//! service is there for an admin only, and an acknowledged need is there with
//! `acked: true`. The list is per user, so it is the document's, not an
//! entry's.
//!
//! **The stream.** One event name, `values`, whose data is
//! `{ now, full, services, removed, services_add, services_visible?, needs? }`:
//! the first event of a stream has `full: true` and holds every entry (the
//! page replaces what it has); after that an event holds the entries that
//! changed since the last one and, in `removed`, the ids whose *values* the
//! user no longer gets (the service was deleted, hidden from them or is no
//! longer bound). `removed` says nothing of the page's list of services (the
//! editor's picker): a service that is visible but unbound is still offered.
//! That list follows `services_visible`, every id the user may see, sent when
//! it is the first event or the set has changed (a service was hidden or
//! deleted: drop the ids not in it), and `services_add`, the views (the
//! `#mm-services` shape: id, name, kind, interval, metrics with labels) of
//! the visible services this stream has not sent one of yet or whose view has
//! changed since (a rename; all of them in the first event), so that a service
//! that became visible after the page was made can be offered and drawn, and
//! one that was renamed is offered under its new name; both are built from the visibility list
//! alone, so they never name a hidden service. `needs` is the document's
//! list, sent when it is the first event or it has changed (also when only an
//! acknowledgement did). The server remembers what it sent, so a
//! change is compared with the document the client actually has, not guessed
//! from which service announced it.
//!
//! History is sent whole once: in the first event and after a `Lagged` (both
//! are `full: true`), as `history` and `history_daily`, replacing what the
//! page has. In every other event an entry has no `history`; it has
//! `history_append`, the one new point of each metric that has one. The page
//! adds it to the end of its series, or, when the series' last point has the
//! same `unix_s`, replaces that point's value (a service polled faster than
//! every 5 minutes rewrites the point of the current slot). It also drops
//! points older than 24 hours. An entry of a delta does not replace the page's
//! entry as a whole: its history fields are merged as just said. A metric that
//! becomes bound while the stream is open, or one that gained several points
//! between two events, arrives as `history` (and `history_daily`) for that
//! metric alone in a delta: replace that metric's series. So does a point that
//! was rewritten while a newer slot started between two events (an append of
//! the new slot alone would leave the page with the old value).
//!
//! A stream that stays open is kept right across days: when the UTC day
//! changes, the next event sends `history` and `history_daily` again for every
//! bound metric, as a full event does (without `full: true`); and every 15
//! minutes ([`DAILY_REFRESH_S`]) an event also carries `history_daily` for the
//! bound metrics, because today's row keeps changing. A page replaces the
//! daily rows of a metric whenever it has some. The visibility and the layout are read
//! again at every wake, the account too: a signed-out, disabled or demoted
//! user loses the stream or the services.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::{Extension, State};
use axum::response::sse::Event;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::stream;
use serde::Serialize;
use tokio::sync::{broadcast, broadcast::error::RecvError, watch};
use tokio::task::JoinHandle;
use vole_kit::auth::{AuthMethod, CurrentUser};
use vole_kit::db::{DbError, users};
use vole_kit::http::ApiError;
use vole_kit::http::live::{self, Changes, StreamLimit};
use vole_kit::i18n;
use vole_kit::state::Core;

use crate::Magpie;
use crate::history::{self, Day, Point};
use crate::integrations::poller::{Change, PollerHandle};
use crate::integrations::{self, Block, Status, Value};
use crate::layout::{self, Layout};
use crate::needs::{self, NeedView};
use crate::services::{ServiceEvent, ServiceSummary, ServiceView, Viewer, store, view_of};

/// Streams open at once, across all users.
pub const MAX_STREAMS: usize = 64;
/// The quiet period after a change: a burst of readings is one event.
pub const QUIET: Duration = Duration::from_secs(1);

/// How often a stream sends the daily rows again (seconds): today's row keeps
/// changing while the stream is open.
pub const DAILY_REFRESH_S: i64 = 900;

/// Streams one user may have open at once (the tabs of one person).
pub const MAX_STREAMS_PER_USER: usize = 8;

static STREAMS: StreamLimit = StreamLimit::new(MAX_STREAMS);
/// Open streams per (running Magpie, user). A server runs one Magpie, so this
/// is per user; the key's first part tells apart the several Magpies of one
/// test process, whose users number from 1 each, so that the streams of one
/// test never count against another's.
static USER_STREAMS: std::sync::LazyLock<std::sync::Mutex<HashMap<(usize, i64), usize>>> =
    std::sync::LazyLock::new(Default::default);

/// One of a user's places under [`MAX_STREAMS_PER_USER`], given back on drop.
struct UserSlot((usize, i64));

impl UserSlot {
    fn acquire(instance: usize, user_id: i64) -> Option<UserSlot> {
        let key = (instance, user_id);
        let mut open = USER_STREAMS.lock().unwrap_or_else(|e| e.into_inner());
        let n = open.entry(key).or_insert(0);
        if *n >= MAX_STREAMS_PER_USER {
            return None;
        }
        *n += 1;
        Some(UserSlot(key))
    }
}

impl Drop for UserSlot {
    fn drop(&mut self) {
        let mut open = USER_STREAMS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = open.get_mut(&self.0) {
            *n -= 1;
            if *n == 0 {
                open.remove(&self.0);
            }
        }
    }
}

/// One service as a page or the extension sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ServiceValues {
    pub name: String,
    pub kind: String,
    pub interval_s: i64,
    pub status: Status,
    pub values: BTreeMap<String, Value>,
    /// What the card draws next to its numbers; absent when there is nothing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<Block>,
    /// Unix seconds, `null` while the service has no snapshot.
    pub fetched_at: Option<i64>,
    /// Why the reading failed, translated; absent when it did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_label: Option<String>,
    /// The last 24 hours of each bound metric (see the module).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub history: BTreeMap<String, Vec<Point>>,
    /// The last 30 days of each bound metric (see the module).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub history_daily: BTreeMap<String, Vec<Day>>,
    /// In a stream event: the point to add to each metric's series.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub history_append: BTreeMap<String, Point>,
}

/// The values of every bound, visible service, by service id (as text, the
/// way JSON keys are).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValuesDoc {
    /// Unix seconds on the server's clock, for the page's age arithmetic.
    pub now: i64,
    pub services: BTreeMap<String, ServiceValues>,
    /// The needs of those services for this user, acknowledgements applied
    /// (see [`crate::needs`]).
    pub needs: Vec<NeedView>,
}

/// The services the layout's cards are bound to, each with the metrics its
/// cards show (all cards of a service together).
fn bound_metrics(layout: &Layout) -> BTreeMap<i64, BTreeSet<String>> {
    let mut bound: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
    for service in layout
        .groups
        .iter()
        .flat_map(|g| &g.cards)
        .filter_map(|c| c.service.as_ref())
    {
        bound
            .entry(service.id)
            .or_default()
            .extend(service.metrics.iter().cloned());
    }
    bound
}

/// A reason as the page shows it: one of [`integrations::FetchError`]'s fixed
/// texts translated, `HTTP nnn` as it is, anything else as the general
/// "the request failed". Stored text is not trusted to be harmless.
pub fn error_label(text: &str) -> String {
    if let Some(key) = integrations::error_text_key(text) {
        return i18n::t(key).to_string();
    }
    let status = text.strip_prefix("HTTP ");
    if status.is_some_and(|s| s.len() == 3 && s.bytes().all(|b| b.is_ascii_digit())) {
        return text.to_string();
    }
    i18n::t("fetch_err.other").to_string()
}

fn entry(summary: &ServiceSummary, poller: &PollerHandle) -> ServiceValues {
    match poller.snapshot(summary.id) {
        Some(snapshot) => {
            let mut values = snapshot.values.clone();
            integrations::localize_texts(&summary.kind, &mut values);
            ServiceValues {
                name: summary.name.clone(),
                kind: summary.kind.clone(),
                interval_s: summary.interval_s,
                status: snapshot.status,
                values,
                details: snapshot.details.clone(),
                fetched_at: Some(snapshot.fetched_at.as_second()),
                error_label: snapshot.error.as_deref().map(error_label),
                history: BTreeMap::new(),
                history_daily: BTreeMap::new(),
                history_append: BTreeMap::new(),
            }
        }
        None => ServiceValues {
            name: summary.name.clone(),
            kind: summary.kind.clone(),
            interval_s: summary.interval_s,
            status: Status::Unknown,
            values: BTreeMap::new(),
            details: Vec::new(),
            fetched_at: None,
            error_label: None,
            history: BTreeMap::new(),
            history_daily: BTreeMap::new(),
            history_append: BTreeMap::new(),
        },
    }
}

/// The values document of `viewer` for `layout`: the services the layout's
/// cards are bound to, minus those `viewer` may not see (see the module).
/// Labels come in the current request's language.
pub async fn values_for(
    core: &Core,
    poller: &PollerHandle,
    viewer: impl Into<Viewer>,
    layout: &Layout,
) -> Result<ValuesDoc, DbError> {
    let viewer = viewer.into();
    let summaries = bound_visible(core, viewer, layout).await?;
    let mut doc = doc_of(core, viewer, &summaries, poller).await?;
    // The history of what the user's cards show, of the services above only.
    let bound = bound_metrics(layout);
    let mut wanted = BTreeMap::new();
    for (key, entry) in &mut doc.services {
        let Ok(id) = key.parse::<i64>() else { continue };
        let Some(metrics) = bound.get(&id) else {
            continue;
        };
        entry.history = poller.history(id, metrics, doc.now);
        wanted.insert(id, metrics.clone());
    }
    attach_daily(core, &mut doc.services, wanted, doc.now).await?;
    Ok(doc)
}

/// Puts the daily rows of `wanted` (service id to metrics) on their entries.
async fn attach_daily(
    core: &Core,
    entries: &mut BTreeMap<String, ServiceValues>,
    wanted: BTreeMap<i64, BTreeSet<String>>,
    now: i64,
) -> Result<(), DbError> {
    let mut daily = history::daily(core, wanted, now).await?;
    for (key, entry) in entries {
        let Ok(id) = key.parse::<i64>() else { continue };
        if let Some(days) = daily.remove(&id) {
            entry.history_daily = days;
        }
    }
    Ok(())
}

/// The services bound on `layout` that `viewer` may see: the intersection
/// the values and the needs are built from. Nothing outside it is ever named.
pub(crate) async fn bound_visible(
    core: &Core,
    viewer: impl Into<Viewer>,
    layout: &Layout,
) -> Result<Vec<ServiceSummary>, DbError> {
    let bound = bound_metrics(layout);
    if bound.is_empty() {
        return Ok(Vec::new());
    }
    let mut visible = store::visible_to(core, viewer).await?;
    visible.retain(|s| bound.contains_key(&s.id));
    Ok(visible)
}

async fn doc_of(
    core: &Core,
    viewer: Viewer,
    summaries: &[ServiceSummary],
    poller: &PollerHandle,
) -> Result<ValuesDoc, DbError> {
    Ok(ValuesDoc {
        now: poller.now(),
        services: summaries
            .iter()
            .map(|s| (s.id.to_string(), entry(s, poller)))
            .collect(),
        needs: needs::needs_of(core, poller, viewer, summaries).await?,
    })
}

pub fn router() -> Router<Core> {
    Router::new().route("/start/live", get(live_values))
}

// ---------------------------------------------------------------------------
// The stream

/// Stops a task when it is dropped: the stream's, so it goes with the stream.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Wakes one stream (`wake`) for what the poller or the services announce, and
/// for an acknowledgement of `user_id`'s (the others' are not its business).
/// A channel that fell behind is noted in `lagged`: the stream then sends
/// everything again. Ends when the stream is gone or every source is closed.
async fn forward(
    mut readings: broadcast::Receiver<Change>,
    mut services: broadcast::Receiver<ServiceEvent>,
    mut acks: broadcast::Receiver<i64>,
    user_id: i64,
    wake: broadcast::Sender<()>,
    lagged: Arc<AtomicBool>,
) {
    let (mut readings_open, mut services_open, mut acks_open) = (true, true, true);
    while readings_open || services_open || acks_open {
        let lag = tokio::select! {
            r = readings.recv(), if readings_open => match r {
                Ok(_) => false,
                Err(RecvError::Lagged(_)) => true,
                Err(RecvError::Closed) => { readings_open = false; continue }
            },
            r = services.recv(), if services_open => match r {
                Ok(_) => false,
                Err(RecvError::Lagged(_)) => true,
                Err(RecvError::Closed) => { services_open = false; continue }
            },
            r = acks.recv(), if acks_open => match r {
                Ok(id) if id == user_id => false,
                Ok(_) => continue,
                Err(RecvError::Lagged(_)) => true,
                Err(RecvError::Closed) => { acks_open = false; continue }
            },
        };
        if lag {
            lagged.store(true, Ordering::Release);
        }
        if wake.send(()).is_err() {
            return;
        }
    }
}

struct Open {
    core: Core,
    poller: PollerHandle,
    user_id: i64,
    session_version: i64,
    changes: Changes<()>,
    /// What the client has: each entry as it was sent.
    sent: HashMap<i64, String>,
    /// The services the user could see at the last wake, each with the view this
    /// stream sent of it (`services_add`, serialised); an id leaves it when the
    /// service is hidden or deleted, so a view is sent again if it comes back,
    /// and a view that changed (a rename) is sent again.
    viewed: BTreeMap<i64, String>,
    /// The newest point of each metric's series as last sent (whole or as an
    /// append): what the next event's new points are measured against.
    sent_history: HashMap<i64, BTreeMap<String, Point>>,
    /// The UTC day (`YYYY-MM-DD`) of the last event, empty before the first:
    /// when it changes, every bound metric's history and daily rows go out
    /// again (the old day's last row and the dropped 31st day are stale).
    sent_day: String,
    /// When the daily rows were last sent (Unix seconds): today's row grows
    /// with every reading, so they are sent again at most this often
    /// ([`DAILY_REFRESH_S`]) in a later event.
    daily_at: i64,
    /// The needs list as last sent, serialised.
    sent_needs: String,
    first: bool,
    lagged: Arc<AtomicBool>,
    shutdown: watch::Receiver<bool>,
    _slot: live::StreamSlot,
    _user_slot: UserSlot,
    _forwarder: AbortOnDrop,
}

/// What a wake came to.
enum Turn {
    Send(Event),
    /// Nothing the client does not already have.
    Nothing,
    /// The stream must end (the account is gone, disabled or signed out).
    End,
}

impl Open {
    async fn turn(&mut self, full: bool) -> Turn {
        let (id, version) = (self.user_id, self.session_version);
        let user = match self.core.with_conn(move |c| users::get_user(c, id)).await {
            Ok(Some(u)) if u.active && u.session_version == version => u,
            Ok(_) => return Turn::End,
            Err(err) => {
                tracing::warn!(%err, "live values: could not read the user; ending the stream");
                return Turn::End;
            }
        };
        let viewer = Viewer::new(user.id, user.role.is_admin());
        let read = async {
            let (layout, _) = layout::load_or_default(&self.core, id).await?;
            let visible = store::visible_to(&self.core, viewer).await?;
            let bound = bound_metrics(&layout);
            let summaries: Vec<ServiceSummary> = visible
                .iter()
                .filter(|s| bound.contains_key(&s.id))
                .cloned()
                .collect();
            let doc = doc_of(&self.core, viewer, &summaries, &self.poller).await?;
            Ok::<_, DbError>((visible, doc, bound))
        };
        let (visible, doc, bound) = match read.await {
            Ok(read) => read,
            Err(err) => {
                tracing::warn!(%err, "live values: could not read the values; ending the stream");
                return Turn::End;
            }
        };

        // (Not `full || take(..)`: that would leave `first` set.)
        let first = std::mem::take(&mut self.first);
        let full = full || first;
        let mut current: HashMap<i64, String> = HashMap::new();
        let mut entries: BTreeMap<String, ServiceValues> = BTreeMap::new();
        // The history that goes with the entries: a whole series for a metric
        // the client has none of (all of them in a full event) or has missed
        // points of, else only the new point. It is not part of what makes an
        // entry changed (`text`): every reading adds a point, and the readings
        // are in the entry already.
        let mut new_sent: HashMap<i64, BTreeMap<String, Point>> = HashMap::new();
        let mut whole: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
        // A new UTC day sends the history again, as a full event does (the
        // daily rows have moved a day); within a day the daily rows are sent
        // again every quarter of an hour.
        let today = history::day_of(doc.now);
        let day_changed = !self.sent_day.is_empty() && self.sent_day != today;
        let refresh_daily = !full && !day_changed && doc.now - self.daily_at >= DAILY_REFRESH_S;
        let from_scratch = full || day_changed;
        let no_points = BTreeMap::new();
        for (key, value) in &doc.services {
            let Ok(id) = key.parse::<i64>() else { continue };
            let text = serde_json::to_string(value).unwrap_or_default();
            let mut entry = value.clone();
            let mut changed = full || self.sent.get(&id) != Some(&text);
            if let Some(metrics) = bound.get(&id) {
                let known = if from_scratch {
                    &no_points
                } else {
                    self.sent_history.get(&id).unwrap_or(&no_points)
                };
                // The metrics the client has a point of: only what is after
                // that point is looked at (no copy of the series).
                let mut again: BTreeSet<String> = BTreeSet::new();
                for (metric, tail) in self.poller.history_tail(id, known, metrics, doc.now) {
                    let sent = known[&metric];
                    if tail.last == sent {
                        new_sent.entry(id).or_default().insert(metric, sent);
                    } else if tail.newer == 0 && tail.last.at == sent.at {
                        // The slot's point was rewritten: the same time.
                        entry.history_append.insert(metric.clone(), tail.last);
                        new_sent.entry(id).or_default().insert(metric, tail.last);
                        changed = true;
                    } else if tail.newer == 1 && tail.known_value == Some(sent.value) {
                        entry.history_append.insert(metric.clone(), tail.last);
                        new_sent.entry(id).or_default().insert(metric, tail.last);
                        changed = true;
                    } else {
                        // Two or more new points, a point the client has that
                        // was rewritten as well, or a clock that went back:
                        // the series, not a guess.
                        again.insert(metric);
                    }
                }
                // Those and the metrics the client has nothing of (new on a
                // card, or everything in a full event): the whole series.
                again.extend(metrics.iter().filter(|m| !known.contains_key(*m)).cloned());
                for (metric, points) in self.poller.history(id, &again, doc.now) {
                    if let Some(last) = points.last().copied() {
                        new_sent.entry(id).or_default().insert(metric.clone(), last);
                        whole.entry(id).or_default().insert(metric.clone());
                        entry.history.insert(metric, points);
                        changed = true;
                    }
                }
                if from_scratch {
                    // Also a bound metric with daily rows but no point in the
                    // last 24 hours (the service has been quiet): the days
                    // are still worth sending.
                    whole.entry(id).or_default().extend(metrics.iter().cloned());
                    changed = true;
                } else if refresh_daily {
                    let held: Vec<String> = new_sent
                        .get(&id)
                        .into_iter()
                        .flat_map(|m| m.keys().cloned())
                        .collect();
                    if !held.is_empty() {
                        whole.entry(id).or_default().extend(held);
                        changed = true;
                    }
                }
            }
            if changed {
                entries.insert(key.clone(), entry);
            }
            current.insert(id, text);
        }
        if full || day_changed || refresh_daily {
            self.daily_at = doc.now;
        }
        self.sent_day = today;
        if let Err(err) = attach_daily(&self.core, &mut entries, whole, doc.now).await {
            tracing::warn!(%err, "live values: could not read the daily history; ending the stream");
            return Turn::End;
        }
        self.sent_history = new_sent;
        let mut removed: Vec<i64> = if full {
            Vec::new()
        } else {
            self.sent
                .keys()
                .filter(|id| !current.contains_key(id))
                .copied()
                .collect()
        };
        removed.sort_unstable();
        // The views the client has not been given: the services list it holds
        // (the page's `#mm-services`) may lack a service that has become visible.
        let views: Vec<(i64, ServiceView, String)> = visible
            .iter()
            .map(|s| {
                let view = view_of(s);
                let text = serde_json::to_string(&view).unwrap_or_default();
                (s.id, view, text)
            })
            .collect();
        let add: Vec<&ServiceView> = views
            .iter()
            .filter(|(id, _, text)| full || self.viewed.get(id) != Some(text))
            .map(|(_, view, _)| view)
            .collect();
        let now_visible: BTreeSet<i64> = views.iter().map(|(id, _, _)| *id).collect();
        let visible_changed = full || !self.viewed.keys().copied().eq(now_visible.iter().copied());
        let needs_text = serde_json::to_string(&doc.needs).unwrap_or_default();
        let needs_changed = full || self.sent_needs != needs_text;
        self.viewed = views
            .iter()
            .map(|(id, _, text)| (*id, text.clone()))
            .collect();
        self.sent = current;
        self.sent_needs = needs_text;
        if !full
            && entries.is_empty()
            && removed.is_empty()
            && add.is_empty()
            && !visible_changed
            && !needs_changed
        {
            return Turn::Nothing;
        }
        let mut data = serde_json::json!({
            "now": doc.now,
            "full": full,
            "services": entries,
            "removed": removed,
            "services_add": add,
        });
        if visible_changed {
            data["services_visible"] = serde_json::json!(now_visible);
        }
        if needs_changed {
            data["needs"] = serde_json::json!(doc.needs);
        }
        Turn::Send(Event::default().event("values").data(data.to_string()))
    }
}

/// `GET /start/live`: the signed-in session's values, as events (see the
/// module). Only a session opens it, as with the page itself.
async fn live_values(
    State(core): State<Core>,
    Extension(magpie): Extension<Magpie>,
    user: CurrentUser,
    method: Option<Extension<AuthMethod>>,
) -> Response {
    if !matches!(method, Some(Extension(AuthMethod::Session))) {
        return ApiError::Unauthorized.into_response();
    }
    let Some(slot) = STREAMS.acquire() else {
        tracing::warn!(
            max = MAX_STREAMS,
            "too many live value streams; refusing a new one"
        );
        return live::busy();
    };
    let id = user.id;
    let Some(user_slot) = UserSlot::acquire(magpie.instance, id) else {
        tracing::warn!(user = id, "too many live value streams for one user");
        return live::busy();
    };
    let session_version = match core.with_conn(move |c| users::get_user(c, id)).await {
        Ok(Some(u)) if u.active => u.session_version,
        Ok(_) => return ApiError::Unauthorized.into_response(),
        Err(err) => return err.into_response(),
    };

    // Subscribe before the first document is read, so nothing between the two
    // is missed.
    let (wake, wake_rx) = broadcast::channel(1);
    let lagged = Arc::new(AtomicBool::new(false));
    let forwarder = tokio::spawn(forward(
        magpie.poller.subscribe(),
        magpie.service_events.subscribe(),
        magpie.ack_events.subscribe(),
        id,
        wake,
        lagged.clone(),
    ));
    let shutdown = magpie.shutdown_signal();

    let state = Open {
        core,
        poller: magpie.poller.clone(),
        user_id: id,
        session_version,
        changes: Changes::all(wake_rx).quiet(QUIET),
        sent: HashMap::new(),
        sent_history: HashMap::new(),
        sent_day: String::new(),
        daily_at: 0,
        viewed: BTreeMap::new(),
        sent_needs: String::new(),
        first: true,
        lagged,
        shutdown,
        _slot: slot,
        _user_slot: user_slot,
        _forwarder: AbortOnDrop(forwarder),
    };
    // The stream is polled after this handler returned, outside the request's
    // language scope: carry the language along.
    let lang = i18n::current();
    let events = stream::unfold(state, move |mut st| async move {
        loop {
            let full = if st.first {
                true
            } else {
                let stopped = async {
                    let _ = st.shutdown.wait_for(|stop| *stop).await;
                };
                tokio::select! {
                    changed = st.changes.next() => {
                        changed?;
                        st.lagged.swap(false, Ordering::AcqRel)
                    }
                    _ = stopped => return None,
                }
            };
            match i18n::scope(lang, st.turn(full)).await {
                Turn::Send(event) => return Some((event, st)),
                Turn::Nothing => {}
                Turn::End => return None,
            }
        }
    });
    live::sse(events)
}
