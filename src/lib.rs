//! Mindful Magpie: a self-hosted start page for your links and services.
//!
//! Sign-in, setup, users, API tokens, Security, General and the API reference
//! come from vole-kit. This crate is the skeleton: the product configuration,
//! the database with its own migration set, the process configuration and the
//! router, which mounts the kit's routers (see [`router`]).

#![forbid(unsafe_code)]

pub mod config;
pub mod db;
pub mod layout;
pub mod pages;
pub mod product;
pub mod server;

use std::path::Path;

use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::get;
use axum::{Extension, Router};
use vole_kit::auth::{guard, pages as kit_pages};
use vole_kit::db::settings;
use vole_kit::http::headers;
use vole_kit::state::Core;

pub use product::{VERSION, install};

/// Why Magpie could not start.
pub type StartError = Box<dyn std::error::Error + Send + Sync>;

/// A running Magpie: the kit's state.
#[derive(Clone)]
pub struct Magpie {
    pub core: Core,
}

/// Installs the configuration, opens and migrates the database in
/// `data_dir` and returns the state. `public_url` (`MM_PUBLIC_URL`), when
/// given, is stored in the `base_url` setting, which the kit's same-origin
/// checks and the API examples read; without one the setting keeps what the
/// General page saved.
pub async fn start(data_dir: &Path, public_url: Option<&str>) -> Result<Magpie, StartError> {
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
    Ok(Magpie { core })
}

/// The whole app: Magpie's pages, the kit's pages, static files and the
/// health probe, behind the kit's middleware.
pub fn router(magpie: Magpie) -> Router {
    let core = magpie.core.clone();
    // Pages that need a signed-in user: Magpie's own, then the kit's
    // (settings, users, tokens, API reference). Sign-in guards them all.
    let protected = Router::new()
        .merge(pages::router())
        .merge(kit_pages::router())
        .route_layer(from_fn_with_state(core.clone(), guard::require_auth));
    // Sign-in, first-run setup, logout, health and static files.
    let public = Router::new()
        .route("/healthz", get(vole_kit::http::healthz))
        .route("/static/{*path}", get(vole_kit::http::assets::static_asset))
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
