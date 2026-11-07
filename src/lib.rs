//! Mindful Magpie: a self-hosted start page for your links and services.
//!
//! Sign-in, setup, users, API tokens, Security, General and the API reference
//! come from vole-kit. This crate is the skeleton: the product configuration,
//! the database with its own migration set, the process configuration and the
//! router, which mounts the kit's routers (see [`router`]).

#![forbid(unsafe_code)]

pub mod api;
pub mod config;
pub mod connect;
pub mod db;
pub mod integrations;
pub mod layout;
pub mod needs;
pub mod pages;
pub mod product;
pub mod server;
pub mod services;
pub mod values;

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::get;
use axum::{Extension, Router};
use vole_kit::auth::{guard, pages as kit_pages};
use vole_kit::db::settings;
use vole_kit::http::headers;
use vole_kit::http::ratelimit::TokenBuckets;
use vole_kit::state::Core;

pub use product::{VERSION, install};

/// The next [`Magpie::instance`].
static NEXT_INSTANCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

/// Why Magpie could not start.
pub type StartError = Box<dyn std::error::Error + Send + Sync>;

/// A running Magpie: the kit's state.
#[derive(Clone)]
pub struct Magpie {
    pub core: Core,
    /// Budget of the extension's token endpoint, per client address.
    pub token_budget: Arc<TokenBuckets<IpAddr>>,
    /// The redirect hosts "Koble til Magpie" may send people back to: the
    /// official extension's and those of `MM_EXTENSION_IDS` (see
    /// [`connect::redirect_host`]).
    pub extension_hosts: Arc<Vec<String>>,
    /// The users' uploaded start page backgrounds (files under the data
    /// directory).
    pub backgrounds: Arc<layout::Backgrounds>,
    /// Announces a service that was created, changed or deleted, after the
    /// change is stored. The poller subscribes; with no subscriber a send is
    /// dropped.
    pub service_events: tokio::sync::broadcast::Sender<services::ServiceEvent>,
    /// The latest snapshot of every service (see [`integrations::poller`]).
    /// Idle until the server starts the poller; pages only read from it.
    pub poller: integrations::poller::PollerHandle,
    /// Tells this running Magpie from the others of one process (the tests'):
    /// what the per-user stream cap is counted under (see [`values`]).
    pub instance: usize,
    /// Announces the id of a user who acknowledged a need or took an
    /// acknowledgement back, so that their open streams (see [`values`])
    /// send the new list. With no stream a send is dropped.
    pub ack_events: tokio::sync::broadcast::Sender<i64>,
    /// Set when the server is told to stop: open event streams (see
    /// [`values`]) end, because the server waits for every connection to
    /// finish before it exits.
    pub shutdown: Arc<tokio::sync::watch::Sender<bool>>,
}

impl Magpie {
    /// Tells the open event streams to end (the server calls it when it is
    /// told to stop).
    pub fn begin_shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    /// A receiver that turns `true` when [`Magpie::begin_shutdown`] is called.
    pub fn shutdown_signal(&self) -> tokio::sync::watch::Receiver<bool> {
        self.shutdown.subscribe()
    }
}

/// Installs the configuration, opens and migrates the database in
/// `data_dir` and returns the state. `public_url` (`MM_PUBLIC_URL`), when
/// given, is stored in the `base_url` setting, which the kit's same-origin
/// checks and the API examples read; without one the setting keeps what the
/// General page saved.
pub async fn start(data_dir: &Path, public_url: Option<&str>) -> Result<Magpie, StartError> {
    start_with(data_dir, public_url, &[]).await
}

/// [`start`], and `extension_ids` (`MM_EXTENSION_IDS`): extension IDs besides
/// the official one that may use the connect flow.
pub async fn start_with(
    data_dir: &Path,
    public_url: Option<&str>,
    extension_ids: &[String],
) -> Result<Magpie, StartError> {
    install();
    std::fs::create_dir_all(data_dir).map_err(|e| {
        format!(
            "can't create the data directory {}: {e}",
            data_dir.display()
        )
    })?;
    let pool = vole_kit::db::build_pool(&data_dir.join(product::DB_FILE));
    let report = vole_kit::db::migrate(&pool, &data_dir.join("backups")).await?;
    if !report.applied.is_empty() {
        tracing::info!(applied = ?report.applied, "database migrated");
    }
    let http = reqwest::Client::builder()
        .user_agent(product::USER_AGENT)
        .build()?;
    let core = Core::new(
        pool,
        vole_kit::auth::load_or_create_key(data_dir),
        http,
        &product::KIT,
    );
    if let Some(url) = public_url.map(str::to_string) {
        core.with_conn(move |c| settings::set(c, settings::BASE_URL, &url))
            .await?;
    }
    // Files of users who were deleted while Magpie was not running, and
    // leftovers of an upload that did not finish.
    let backgrounds = Arc::new(layout::Backgrounds::new(data_dir));
    if let Err(err) = backgrounds.reconcile(&core).await {
        tracing::warn!("can't sweep the background files: {err}");
    }
    Ok(Magpie {
        core,
        token_budget: Arc::new(TokenBuckets::new()),
        extension_hosts: Arc::new(connect::allowed_hosts(extension_ids)),
        backgrounds,
        service_events: tokio::sync::broadcast::channel(64).0,
        poller: integrations::poller::PollerHandle::idle(),
        instance: NEXT_INSTANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ack_events: tokio::sync::broadcast::channel(64).0,
        shutdown: Arc::new(tokio::sync::watch::channel(false).0),
    })
}

/// The whole app: Magpie's pages, the kit's pages, static files and the
/// health probe, behind the kit's middleware.
pub fn router(magpie: Magpie) -> Router {
    let core = magpie.core.clone();
    // Pages that need a signed-in user: Magpie's own, then the kit's
    // (settings, users, tokens, API reference). Sign-in guards them all.
    let protected = Router::new()
        .merge(pages::router())
        .merge(connect::router())
        .merge(api::router())
        .merge(services::pages::router())
        .merge(values::router())
        .merge(needs::router())
        .merge(kit_pages::router())
        // Inside the sign-in guard: sees who is signed in (see the function).
        .route_layer(from_fn(pages::sweep_after_user_delete))
        .route_layer(from_fn_with_state(core.clone(), guard::require_auth));
    // Sign-in, first-run setup, logout, health and static files.
    let public = Router::new()
        .route("/healthz", get(vole_kit::http::healthz))
        .route("/static/{*path}", get(vole_kit::http::assets::static_asset))
        .merge(api::public_router())
        // Takes a cookie or an API token and answers 401 itself (see the module).
        .merge(pages::image_router())
        .merge(kit_pages::public_router());
    protected
        .merge(public)
        .fallback(vole_kit::http::error::not_found)
        .layer(Extension(magpie))
        .layer(from_fn_with_state(core.clone(), guard::csrf_guard))
        .layer(from_fn_with_state(core.clone(), vole_kit::i18n::layer))
        .layer(from_fn(vole_kit::http::error::negotiate))
        .layer(from_fn(headers::security_headers))
        .with_state(core)
}
