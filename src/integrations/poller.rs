//! The background poller: fetches every service on its interval, keeps the
//! latest [`Snapshot`] of each one in memory and in `service_snapshots`, and
//! announces each new snapshot.
//!
//! Pages and the extension never fetch. They read [`PollerHandle::snapshot`],
//! which is an in-memory lookup, so a slow or dead service can't slow them.
//!
//! **Shape.** [`Poller::start`] loads the stored snapshots (so a restart
//! shows the last values, with their own age, before the first fetch, and a
//! request right after the server binds already sees them) and then spawns
//! one supervisor task, which runs one task per service. The supervisor
//! listens for [`ServiceEvent`]s (the services pages announce a created,
//! changed or deleted service on `Magpie::service_events`, after the change is
//! stored) and for [`PollerHandle::reload`]; a
//! changed service gets a new task that fetches at once, a deleted one loses
//! its task, its snapshot and its row. Fetches run in a task of their own with
//! a deadline, so a hung or panicking kind costs its own service one bad
//! reading and nothing else.
//!
//! **Schedule.** The first fetch after start is staggered over
//! [`START_SPREAD`] so a restart does not hit every service at once. After
//! that a service is fetched every `interval` (the service's, at least the
//! kind's minimum), ±10 %. A failed fetch (no reading, or the deadline, or a
//! panic) doubles the wait, up to [`MAX_BACKOFF`] (or the interval when that
//! is longer); a reading resets it. A bad reading, even one that says why (a
//! site that answers 500, or not at all), is the service's state, not a
//! failure of the fetch: it is read again at the normal interval.
//!
//! **«Trenger deg».** A kind sees one fetch at a time, so "bad twice in a row"
//! is the poller's: from the second bad reading in a row a service gets a
//! need. It is [`DOWN_KEY`] («the site does not answer») for a kind that has
//! the rule (see [`down_label`]), or [`CONFIG_KEY`] («set it up again») when
//! the reading failed because the settings, the address or the stored secret
//! can't be used (an address that is not allowed, a redirect to another
//! scheme): that needs an admin, not the site. The count survives a restart, an update
//! and a resync: it is taken up again from the stored snapshot, so a need
//! that is showing does not go and come back (an acknowledgement would
//! reopen).
//!
//! **Secrets.** The sealed secret is opened for the length of one fetch, in
//! [`super::FetchCtx`] (whose `Debug` hides it). Nothing here logs settings,
//! an address or a secret: log lines carry the service id, the kind and a
//! [`FetchError`] text, which is a fixed set.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rusqlite::params;
use tokio::sync::{broadcast, mpsc};
use tokio::task::{AbortHandle, JoinSet};
use vole_kit::db;
use vole_kit::state::Core;

use super::{FetchError, Integration, Level, Need, Snapshot, Status};
use crate::services::{Service, ServiceEvent, store};

/// How long one fetch may take before the poller gives up on it. The HTTP
/// client's own timeout is shorter; this is for a kind that waits on
/// something else.
pub const FETCH_DEADLINE: Duration = Duration::from_secs(30);
/// The longest wait after failures (unless the interval itself is longer).
pub const MAX_BACKOFF: Duration = Duration::from_secs(600);
/// The first fetches after start are spread over this long.
pub const START_SPREAD: Duration = Duration::from_secs(5);
/// How far the wait varies: ±10 %.
const JITTER: f64 = 0.1;
/// Bad readings in a row before the service needs the user.
const DOWN_AFTER: u32 = 2;
/// The key of the need «bad twice in a row»; what an acknowledgement refers to.
pub const DOWN_KEY: &str = "down";
/// The key of the need «the settings or the secret can't be used».
pub const CONFIG_KEY: &str = "config";
/// The translation key of the [`CONFIG_KEY`] need, for every kind.
pub const CONFIG_LABEL: &str = "need.service_config";

/// Finds a kind by its stored name.
pub type Kinds = fn(&str) -> Option<&'static dyn Integration>;

/// What a poller needs besides the database: tests change them.
pub struct Options {
    /// Where kinds come from; [`super::find`] unless a test brings its own.
    pub kinds: Kinds,
    /// How long the first fetches after start are spread over.
    pub spread: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            kinds: super::find,
            spread: START_SPREAD,
        }
    }
}

/// The translation key of the need «bad twice in a row» for `kind`, when the
/// kind has the rule. Kinds that know better report their own needs from
/// `fetch`; `http` has nothing but this.
pub fn down_label(kind: &str) -> Option<&'static str> {
    match kind {
        "http" => Some("need.http_down"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Shared state and the handle

#[derive(Default)]
struct State {
    snapshots: HashMap<i64, Arc<Snapshot>>,
    /// The generation of each service's running task: only that task may
    /// publish, so a replaced or removed one can't write after the fact.
    live: HashMap<i64, u64>,
}

struct Shared {
    state: RwLock<State>,
    changes: broadcast::Sender<Change>,
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Shared {
            state: RwLock::new(State::default()),
            changes: broadcast::channel(256).0,
        })
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(|e| e.into_inner())
    }

    fn forget(&self, id: i64) {
        let mut state = self.write();
        state.live.remove(&id);
        state.snapshots.remove(&id);
    }

    /// Stores `snapshot` and announces `id`, unless `generation` is no longer
    /// the service's.
    fn publish(&self, id: i64, generation: u64, snapshot: Arc<Snapshot>) {
        {
            let mut state = self.write();
            if state.live.get(&id) != Some(&generation) {
                return;
            }
            state.snapshots.insert(id, snapshot);
        }
        let _ = self.changes.send(Change::Snapshot(id));
    }
}

/// What [`PollerHandle::subscribe`] announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The service has a new snapshot.
    Snapshot(i64),
    /// The service is gone: no snapshot of it any more.
    Removed(i64),
}

enum Control {
    Reload(i64),
    Remove(i64),
}

/// A running poller, as the pages see it. Cheap to clone; when the last clone
/// is dropped the poller stops.
#[derive(Clone)]
pub struct PollerHandle {
    shared: Arc<Shared>,
    control: Option<mpsc::UnboundedSender<Control>>,
}

impl PollerHandle {
    /// A handle with no poller behind it: it holds only what
    /// [`PollerHandle::set_snapshot_for_tests`] puts in. What `Magpie` starts with until
    /// the server starts the real one, and what tests that need only
    /// snapshots use.
    pub fn idle() -> Self {
        PollerHandle {
            shared: Shared::new(),
            control: None,
        }
    }

    /// Fetches the service again at once, with what is now stored (a new
    /// service, or a changed one). The service events do the same.
    pub fn reload(&self, service_id: i64) {
        if let Some(control) = &self.control {
            let _ = control.send(Control::Reload(service_id));
        }
    }

    /// Forgets the service as if it were deleted: its task stops, its
    /// snapshot and its row go, and [`Change::Removed`] is announced.
    /// [`PollerHandle::snapshot`] answers `None` from the moment this returns.
    /// **For tests only** (hidden from the docs): the pages never call it,
    /// they announce [`ServiceEvent::Deleted`] and the poller follows.
    #[doc(hidden)]
    pub fn remove_for_tests(&self, service_id: i64) {
        self.shared.forget(service_id);
        match &self.control {
            // The poller announces it once the row is gone too.
            Some(control) => {
                let _ = control.send(Control::Remove(service_id));
            }
            None => {
                let _ = self.shared.changes.send(Change::Removed(service_id));
            }
        }
    }

    /// The latest snapshot of the service, if there is one. Never waits.
    pub fn snapshot(&self, service_id: i64) -> Option<Arc<Snapshot>> {
        self.shared.read().snapshots.get(&service_id).cloned()
    }

    /// A [`Change`] each time a service gets a new snapshot or is removed (also
    /// when its deletion arrives as a service event). A receiver that falls far
    /// behind gets `Lagged` and should read every snapshot again.
    pub fn subscribe(&self) -> broadcast::Receiver<Change> {
        self.shared.changes.subscribe()
    }

    /// Puts `snapshot` in as the service's latest, as a fetch would (and
    /// announces it), without storing it. **For tests only** (hidden from the
    /// docs): the integration tests are separate crates, so a `cfg(test)`
    /// would hide it from them too. A running poller replaces it at its next
    /// fetch.
    #[doc(hidden)]
    pub fn set_snapshot_for_tests(&self, service_id: i64, snapshot: Snapshot) {
        {
            let mut state = self.shared.write();
            state.snapshots.insert(service_id, Arc::new(snapshot));
        }
        let _ = self.shared.changes.send(Change::Snapshot(service_id));
    }
}

// ---------------------------------------------------------------------------
// Starting

/// See the module.
pub struct Poller;

impl Poller {
    /// Starts polling every stored service with `http` and follows `events`.
    /// The stored snapshots are in memory when this returns. Needs a running
    /// tokio runtime.
    pub async fn start(
        core: Core,
        http: reqwest::Client,
        events: &broadcast::Sender<ServiceEvent>,
    ) -> PollerHandle {
        Self::start_with(core, http, events, Options::default()).await
    }

    /// [`Poller::start`] with other [`Options`].
    pub async fn start_with(
        core: Core,
        http: reqwest::Client,
        events: &broadcast::Sender<ServiceEvent>,
        options: Options,
    ) -> PollerHandle {
        let shared = Shared::new();
        let (control, commands) = mpsc::unbounded_channel();
        // Subscribed here, before the task runs, so no event is missed.
        let events = events.subscribe();
        let supervisor = Supervisor {
            core,
            http,
            options,
            shared: shared.clone(),
            tasks: JoinSet::new(),
            running: HashMap::new(),
            next_generation: 0,
        };
        supervisor.load_stored().await;
        let run = tokio::spawn(supervisor.run(commands, events));
        // A panic in the supervisor would stop all polling without a word.
        tokio::spawn(async move {
            if let Err(err) = run.await
                && err.is_panic()
            {
                tracing::error!("the poller's supervisor panicked: no service is fetched any more");
            }
        });
        PollerHandle {
            shared,
            control: Some(control),
        }
    }
}

struct Supervisor {
    core: Core,
    http: reqwest::Client,
    options: Options,
    shared: Arc<Shared>,
    /// Dropping the set (when the supervisor ends) stops every service task.
    tasks: JoinSet<()>,
    running: HashMap<i64, Running>,
    next_generation: u64,
}

/// A service's running task, and the service as it was started with.
struct Running {
    abort: AbortHandle,
    service: Service,
}

/// Whether a restart of the task would change nothing: the same kind, name,
/// address, settings, interval and secret (a replaced secret changes
/// `updated_at`).
fn same_job(a: &Service, b: &Service) -> bool {
    a.kind == b.kind
        && a.name == b.name
        && a.url == b.url
        && a.settings == b.settings
        && a.has_secret == b.has_secret
        && a.interval_s == b.interval_s
        && a.updated_at == b.updated_at
}

impl Supervisor {
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Control>,
        events: broadcast::Receiver<ServiceEvent>,
    ) {
        match store::list(&self.core).await {
            Ok(services) => {
                tracing::info!(services = services.len(), "poller started");
                for service in services {
                    let delay = start_delay(self.options.spread);
                    self.spawn(service, delay);
                }
            }
            Err(err) => tracing::warn!(%err, "poller can't list the services"),
        }
        let mut events = Some(events);
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    None => break,
                    Some(Control::Reload(id)) => self.reload(id).await,
                    Some(Control::Remove(id)) => self.remove(id).await,
                },
                event = next_event(&mut events) => match event {
                    Ok(ServiceEvent::Created(id) | ServiceEvent::Updated(id)) => {
                        self.reload(id).await;
                    }
                    Ok(ServiceEvent::Deleted(id)) => self.remove(id).await,
                    Err(broadcast::error::RecvError::Lagged(_)) => self.resync().await,
                    Err(broadcast::error::RecvError::Closed) => events = None,
                },
                Some(done) = self.tasks.join_next(), if !self.tasks.is_empty() => {
                    if let Err(err) = done
                        && err.is_panic()
                    {
                        tracing::error!("a poller task panicked");
                    }
                }
            }
        }
        tracing::debug!("poller stopped");
    }

    /// Puts the stored snapshots in memory, before the first fetch.
    async fn load_stored(&self) {
        let rows = db::interact(&self.core.db, |c| {
            let mut stmt = c.prepare("SELECT service_id, json FROM service_snapshots")?;
            stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()
        })
        .await;
        let rows = match rows {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(%err, "poller can't read the stored snapshots");
                return;
            }
        };
        let mut state = self.shared.write();
        for (id, json) in rows {
            match serde_json::from_str::<Snapshot>(&json) {
                Ok(snapshot) => {
                    state.snapshots.insert(id, Arc::new(snapshot));
                }
                // Left to be replaced by the next fetch.
                Err(_) => tracing::warn!(service_id = id, "poller skips an unreadable snapshot"),
            }
        }
    }

    /// Starts (or restarts) the task of `service`, which waits `delay` before
    /// its first fetch.
    fn spawn(&mut self, service: Service, delay: Duration) {
        let id = service.id;
        if let Some(old) = self.running.remove(&id) {
            old.abort.abort();
        }
        let Some(kind) = (self.options.kinds)(&service.kind) else {
            tracing::warn!(service_id = id, kind = %service.kind, "poller has no such kind");
            self.shared.write().live.remove(&id);
            return;
        };
        self.next_generation += 1;
        let generation = self.next_generation;
        // The count of bad readings carries on from the current snapshot (the
        // stored one after a restart).
        let seed = {
            let mut state = self.shared.write();
            state.live.insert(id, generation);
            state.snapshots.get(&id).map_or(0, |s| bad_seed(s))
        };
        let job = Job {
            core: self.core.clone(),
            http: self.http.clone(),
            shared: self.shared.clone(),
            service,
            kind,
            generation,
            delay,
            seed,
        };
        let running = Running {
            service: job.service.clone(),
            abort: self.tasks.spawn(job.run()),
        };
        self.running.insert(id, running);
    }

    async fn reload(&mut self, id: i64) {
        match store::get(&self.core, id).await {
            Ok(Some(service)) => self.spawn(service, Duration::ZERO),
            Ok(None) => self.remove(id).await,
            Err(err) => tracing::warn!(service_id = id, %err, "poller can't read the service"),
        }
    }

    async fn remove(&mut self, id: i64) {
        if let Some(task) = self.running.remove(&id) {
            task.abort.abort();
        }
        self.shared.forget(id);
        // Normally the row went with the service already.
        let deleted = db::interact(&self.core.db, move |c| {
            c.execute(
                "DELETE FROM service_snapshots WHERE service_id = ?1",
                params![id],
            )
        })
        .await;
        if let Err(err) = deleted {
            tracing::warn!(service_id = id, %err, "poller can't delete a stored snapshot");
        }
        let _ = self.shared.changes.send(Change::Removed(id));
    }

    /// After missed events: a service that is new, or whose settings, interval
    /// or secret changed, gets a fresh task (after the start-up stagger, since
    /// many may have changed); one that is unchanged keeps its task, with its
    /// schedule, backoff and count of bad readings. A task of a service that is
    /// gone stops.
    async fn resync(&mut self) {
        let services = match store::list(&self.core).await {
            Ok(services) => services,
            Err(err) => {
                tracing::warn!(%err, "poller can't list the services");
                return;
            }
        };
        let stored: HashSet<i64> = services.iter().map(|s| s.id).collect();
        let gone: Vec<i64> = self
            .running
            .keys()
            .filter(|id| !stored.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.remove(id).await;
        }
        for service in services {
            if let Some(running) = self.running.get(&service.id)
                && same_job(&running.service, &service)
            {
                continue;
            }
            let delay = start_delay(self.options.spread);
            self.spawn(service, delay);
        }
    }
}

/// The next service event, or never once the sender is gone.
async fn next_event(
    events: &mut Option<broadcast::Receiver<ServiceEvent>>,
) -> Result<ServiceEvent, broadcast::error::RecvError> {
    match events {
        Some(events) => events.recv().await,
        None => std::future::pending().await,
    }
}

// ---------------------------------------------------------------------------
// One service

struct Job {
    core: Core,
    http: reqwest::Client,
    shared: Arc<Shared>,
    service: Service,
    kind: &'static dyn Integration,
    generation: u64,
    delay: Duration,
    /// Bad readings in a row so far (see [`bad_seed`]).
    seed: u32,
}

/// Aborts a task when dropped, so a fetch does not outlive the loop that
/// waits for it.
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Job {
    async fn run(self) {
        let interval = self.interval();
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        // Failed fetches in a row (they back the schedule off), and bad
        // readings in a row (they make the need).
        let mut failures: u32 = 0;
        let mut bad_in_a_row: u32 = self.seed;
        loop {
            let Some((mut snapshot, failed)) = self.fetch().await else {
                // Changed while this task waited: the new task of the service
                // is on its way (the service event), this one only waits.
                tokio::time::sleep(jitter(interval)).await;
                continue;
            };
            failures = if failed { failures + 1 } else { 0 };
            bad_in_a_row = if snapshot.status == Status::Bad {
                bad_in_a_row + 1
            } else {
                0
            };
            if bad_in_a_row >= DOWN_AFTER
                && snapshot.status == Status::Bad
                && let Some(need) = bad_need(&self.service, &snapshot)
            {
                snapshot.needs.push(need);
            }
            let before = self.shared.read().snapshots.get(&self.service.id).cloned();
            date_needs(&mut snapshot, before.as_deref());
            self.clear_acks(&snapshot, before.as_deref()).await;
            tracing::debug!(
                service_id = self.service.id,
                kind = %self.service.kind,
                status = ?snapshot.status,
                error = snapshot.error.as_deref().unwrap_or(""),
                "poller fetched"
            );
            self.store(&snapshot).await;
            self.shared
                .publish(self.service.id, self.generation, Arc::new(snapshot));
            tokio::time::sleep(jitter(next_wait(interval, failures))).await;
        }
    }

    /// Deletes everybody's acknowledgements that `snapshot` has spent: those of
    /// a need that is gone (one that comes back is shown again) and those of a
    /// need that is now worse than when it was acknowledged (its level rose or
    /// its score is higher). An acknowledgement is spent once it stops hiding,
    /// for good: a need that got worse and then eased stays shown. Only when
    /// the needs differ from the snapshot before it (their keys, levels or
    /// scores), or when there is none to compare with.
    async fn clear_acks(&self, snapshot: &Snapshot, before: Option<&Snapshot>) {
        if before.is_some_and(|before| need_marks(before) == need_marks(snapshot)) {
            return;
        }
        let needs = snapshot
            .needs
            .iter()
            .map(|n| (n.key.clone(), n.level, n.score))
            .collect();
        if let Err(err) = crate::needs::clear_spent(&self.core, self.service.id, needs).await {
            tracing::warn!(service_id = self.service.id, %err, "poller can't clear acknowledgements");
        }
    }

    /// The service's interval, never below the kind's minimum.
    fn interval(&self) -> Duration {
        match u64::try_from(self.service.interval_s) {
            Ok(secs) if secs > 0 => Duration::from_secs(secs).max(self.kind.min_interval()),
            _ => self.kind.default_interval(),
        }
    }

    /// One fetch: the reading, and whether the fetch failed (no reading, a
    /// timeout or a panic; a bad reading is not a failure). `None` when the
    /// service was changed after this task started: its replacement fetches.
    async fn fetch(&self) -> Option<(Snapshot, bool)> {
        let mut ctx = match store::fetch_ctx(&self.core, &self.service).await {
            Ok(ctx) => ctx,
            Err(store::CtxError::Changed) => return None,
            Err(err) => {
                tracing::warn!(
                    service_id = self.service.id,
                    kind = %self.service.kind,
                    %err,
                    "poller can't prepare a fetch"
                );
                return Some((Snapshot::failed(&err.fetch_error()), true));
            }
        };
        ctx.http = self.http.clone();
        let kind = self.kind;
        let fetch = tokio::spawn(async move { kind.fetch(&ctx).await });
        let _stop = AbortOnDrop(fetch.abort_handle());
        Some(match tokio::time::timeout(FETCH_DEADLINE, fetch).await {
            // A reading is the service's state, also a bad one that says why
            // (a site that answers 500 or not at all): it is fetched again at
            // the normal interval, not backed off from.
            Ok(Ok(Ok(snapshot))) => (snapshot, false),
            Ok(Ok(Err(err))) => (Snapshot::failed(&err), true),
            Ok(Err(err)) => {
                if err.is_panic() {
                    tracing::error!(
                        service_id = self.service.id,
                        kind = %self.service.kind,
                        "a fetch panicked"
                    );
                }
                (Snapshot::failed(&FetchError::Other), true)
            }
            Err(_) => (Snapshot::failed(&FetchError::Timeout), true),
        })
    }

    /// Writes the snapshot over the service's row. A service that is gone gets
    /// none, whatever a late fetch brings.
    async fn store(&self, snapshot: &Snapshot) {
        let id = self.service.id;
        let json = match serde_json::to_string(snapshot) {
            Ok(json) => json,
            Err(_) => {
                tracing::warn!(service_id = id, "poller can't encode a snapshot");
                return;
            }
        };
        let fetched_at = snapshot.fetched_at.to_string();
        let stored = db::interact(&self.core.db, move |c| {
            c.execute(
                "INSERT INTO service_snapshots (service_id, json, fetched_at)
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM services WHERE id = ?1)
                 ON CONFLICT (service_id)
                 DO UPDATE SET json = excluded.json, fetched_at = excluded.fetched_at",
                params![id, json, fetched_at],
            )
        })
        .await;
        if let Err(err) = stored {
            tracing::warn!(service_id = id, %err, "poller can't store a snapshot");
        }
    }
}

/// Gives each need of `snapshot` the time it began: the one it has, else the
/// one the same need had in the reading before, else the time of this reading.
/// A need that was not in the reading before begins now, also when it was
/// there earlier and went.
///
/// Also makes the needs safe to store and compare: a `since` after the reading
/// is cut back to the reading (a kind's clock, or a made-up date, must not make
/// an acknowledgement stop hiding), and a score that is not a finite number
/// becomes 0.
fn date_needs(snapshot: &mut Snapshot, before: Option<&Snapshot>) {
    let fetched_at = snapshot.fetched_at;
    for need in &mut snapshot.needs {
        if !need.score.is_finite() {
            need.score = 0.0;
        }
        if let Some(since) = need.since {
            need.since = Some(since.min(fetched_at));
            continue;
        }
        let carried = before.and_then(|b| {
            b.needs
                .iter()
                .find(|n| n.key == need.key)
                .map(|n| n.since.unwrap_or(b.fetched_at))
        });
        need.since = Some(carried.unwrap_or(fetched_at));
    }
}

/// What `clear_acks` compares between two readings: each need's key, level
/// and score (as bits, so the comparison is exact).
fn need_marks(snapshot: &Snapshot) -> Vec<(&str, Level, u64)> {
    snapshot
        .needs
        .iter()
        .map(|n| (n.key.as_str(), n.level, n.score.to_bits()))
        .collect()
}

/// The failures that an admin has to fix, not the site: settings that can't
/// be used, an address that is not allowed, a redirect that is not allowed
/// and a secret that can't be opened.
const CONFIG_ERRORS: [FetchError; 4] = [
    FetchError::Config,
    FetchError::Forbidden,
    FetchError::RedirectScheme,
    FetchError::SecretUnreadable,
];

/// Whether the reading failed because of one of [`CONFIG_ERRORS`].
fn is_config_error(snapshot: &Snapshot) -> bool {
    snapshot
        .error
        .as_deref()
        .is_some_and(|text| CONFIG_ERRORS.iter().any(|e| e.to_string() == text))
}

/// The need of a service that has had bad readings in a row, if its kind has
/// one for this reading.
fn bad_need(service: &Service, snapshot: &Snapshot) -> Option<Need> {
    let (key, label_key) = if is_config_error(snapshot) {
        (CONFIG_KEY, CONFIG_LABEL)
    } else {
        (DOWN_KEY, down_label(&service.kind)?)
    };
    Some(Need {
        key: key.to_string(),
        level: Level::Bad,
        label_key: label_key.to_string(),
        params: BTreeMap::new(),
        // Constant: the service is down or it is not, so an acknowledgement
        // is not undone by the next fetch.
        score: 1.0,
        since: None,
    })
}

/// How many bad readings in a row a snapshot stands for, when the poller
/// takes up the count again (after a restart, or when a service is
/// updated): one less than the need needs if it is already showing one, so
/// the next bad reading keeps it; one if it is merely bad; else none.
fn bad_seed(snapshot: &Snapshot) -> u32 {
    if snapshot
        .needs
        .iter()
        .any(|n| n.key == DOWN_KEY || n.key == CONFIG_KEY)
    {
        DOWN_AFTER - 1
    } else if snapshot.status == Status::Bad {
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Time

/// A number in `[0, 1)`.
fn unit() -> f64 {
    let mut bytes = [0u8; 4];
    rand::fill(&mut bytes);
    f64::from(u32::from_le_bytes(bytes)) / (f64::from(u32::MAX) + 1.0)
}

/// How long the first fetch after start waits.
fn start_delay(spread: Duration) -> Duration {
    spread.mul_f64(unit())
}

/// `wait` ±10 %.
fn jitter(wait: Duration) -> Duration {
    wait.mul_f64(1.0 - JITTER + 2.0 * JITTER * unit())
}

/// The wait before the next fetch, without jitter: the interval, doubled for
/// each failure in a row, up to [`MAX_BACKOFF`] (or the interval, when that
/// is longer already).
fn next_wait(interval: Duration, failures: u32) -> Duration {
    if failures == 0 {
        return interval;
    }
    let cap = MAX_BACKOFF.max(interval);
    interval
        .checked_mul(1u32 << failures.min(20))
        .map_or(cap, |wait| wait.min(cap))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn the_wait_doubles_per_failure_up_to_ten_minutes() {
        assert_eq!(next_wait(s(60), 0), s(60));
        assert_eq!(next_wait(s(60), 1), s(120));
        assert_eq!(next_wait(s(60), 2), s(240));
        assert_eq!(next_wait(s(60), 3), s(480));
        assert_eq!(next_wait(s(60), 4), s(600));
        assert_eq!(next_wait(s(60), 400), s(600));
        // An interval longer than the cap is not shortened by failing.
        assert_eq!(next_wait(s(3600), 3), s(3600));
    }

    #[test]
    fn jitter_stays_within_ten_percent_and_the_start_within_the_spread() {
        for _ in 0..1000 {
            let j = jitter(s(100));
            assert!(j >= s(90) && j <= s(110), "{j:?}");
            assert!(start_delay(s(5)) < s(5));
        }
        assert_eq!(start_delay(Duration::ZERO), Duration::ZERO);
    }
}
