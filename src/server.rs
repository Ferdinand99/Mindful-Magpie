//! Runs the web server until Ctrl-C or SIGTERM, then lets requests in
//! flight finish.

use std::net::SocketAddr;

use crate::config::Config;

/// Starts Magpie with `config` and serves until it is told to stop.
pub async fn run(config: Config) -> Result<(), crate::StartError> {
    let magpie = crate::start_with(
        &config.data_dir,
        config.public_url.as_deref(),
        &config.extension_ids,
    )
    .await?;
    // Notifications and other work outside a request use the site language.
    vole_kit::i18n::site_language(&magpie.core).await;
    // Fetches every service in the background and follows the services pages.
    let poller = crate::integrations::poller::Poller::start(
        magpie.core.clone(),
        crate::integrations::client().clone(),
        &magpie.service_events,
    );
    let magpie = crate::Magpie { poller, ..magpie };

    // Open event streams would keep the graceful shutdown waiting for an hour.
    let stopper = magpie.clone();
    let addr = config.bind_addr();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("can't listen on {addr}: {e}"))?;
    tracing::info!(
        version = crate::VERSION,
        data_dir = %config.data_dir.display(),
        "Mindful Magpie listening on http://{addr}"
    );
    // Connect info lets the rate limiters key on the peer address.
    axum::serve(
        listener,
        crate::router(magpie).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        stopper.begin_shutdown();
    })
    .await?;
    tracing::info!("stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::warn!(%err, "can't listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => {
                tracing::warn!(%err, "can't listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}
