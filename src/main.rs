//! SVAB Slack bot: posts SVAB ticket updates to Slack, using threads.
//!
//! All the work happens in `actor`, which processes commands one at a time.

mod actor;
mod config;

use tokio::sync::mpsc;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::actor::SvabActor;
use crate::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Log level comes from RUST_LOG (e.g. `RUST_LOG=debug`), default `info`.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    info!(?config, "starting svab-slack-bot");

    // Nothing sends commands yet; the HTTP server will hold `tx`.
    let (tx, rx) = mpsc::channel(actor::QUEUE_CAPACITY);
    let actor_task = tokio::spawn(SvabActor::new(rx).run());

    shutdown_signal().await;
    drop(tx);
    actor_task.await?;

    info!("shut down cleanly");
    Ok(())
}

/// Resolve on Ctrl-C (local runs) or SIGTERM (Kubernetes stopping the pod).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for Ctrl-C");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    info!("shutdown signal received");
}
