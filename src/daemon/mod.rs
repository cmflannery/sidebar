//! The sidebar daemon — owns SQLite + broker + scheduler.
//!
//! `sidebar serve` lands here. See ARCHITECTURE.md §3.

pub mod server;
pub mod store;

use std::time::Duration;

use anyhow::Result;
use tokio::signal::unix::{SignalKind, signal};
use tracing::info;

use crate::paths;

const CLEANUP_INTERVAL: Duration = Duration::from_secs(3600);

/// Run the daemon until SIGINT/SIGTERM. Returns Ok(()) on clean shutdown.
pub async fn serve() -> Result<()> {
    paths::ensure_home()?;
    let socket_path = paths::socket()?;
    let db_path = paths::db()?;

    let store = store::Store::open(&db_path).await?;

    // Clean up any sessions that were left open by a previous ungraceful exit.
    let dangling = store.close_dangling_sessions().await?;
    if dangling > 0 {
        info!(
            closed = dangling,
            "closed dangling sessions from previous run"
        );
    }

    // Run a startup cleanup pass.
    let dropped = store.cleanup_old(store::DEFAULT_RETENTION_DAYS).await?;
    if dropped > 0 {
        info!(dropped, "cleanup: dropped fully-read old messages");
    }

    // Hourly cleanup task.
    let cleanup_store = store.clone();
    let cleanup_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        interval.tick().await; // skip the immediate first tick
        loop {
            interval.tick().await;
            match cleanup_store
                .cleanup_old(store::DEFAULT_RETENTION_DAYS)
                .await
            {
                Ok(0) => {}
                Ok(n) => info!(dropped = n, "periodic cleanup"),
                Err(e) => tracing::warn!(error = %e, "periodic cleanup failed"),
            }
        }
    });

    let daemon = server::Daemon::new(store.clone());

    // Run the server until a shutdown signal arrives.
    let shutdown = shutdown_signal();
    let server_result = server::run(daemon, socket_path, shutdown).await;

    cleanup_task.abort();
    // Close any sessions that were still open at shutdown time.
    let closed = store.close_dangling_sessions().await.unwrap_or(0);
    if closed > 0 {
        info!(closed, "closed in-flight sessions on shutdown");
    }
    info!("daemon stopped");

    server_result
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install SIGINT handler");
        }
    };
    let term = async {
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => tracing::error!(error = %e, "failed to install SIGTERM handler"),
        }
    };
    tokio::select! {
        () = ctrl_c => info!("SIGINT received"),
        () = term => info!("SIGTERM received"),
    }
}
