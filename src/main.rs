//! SVAB Slack bot: posts SVAB ticket updates to Slack, using threads.
//!
//! See README.md for the architecture. In short:
//! `server` (HTTP) --Command--> `actor` (does the work, one command at a time),
//! which uses `jira` and `slack` clients, `svab::decide` and `messages`.

mod actor;
mod config;
mod error;
mod jira;
mod messages;
mod server;
mod slack;
mod svab;

use anyhow::{Context, bail};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::actor::SvabActor;
use crate::config::Config;
use crate::jira::JiraClient;
use crate::server::AppState;
use crate::slack::SlackClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Log level comes from RUST_LOG (e.g. `RUST_LOG=debug`), default `info`.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    info!(?config, "starting svab-slack-bot");

    let (tx, rx) = mpsc::channel(actor::QUEUE_CAPACITY);
    let jira = JiraClient::new(config.jira.clone())?;
    // Fail now on bad credentials or missing permissions, with a clear error,
    // rather than on the first webhook.
    let jira_account = jira
        .verify_access(&config.jira_project)
        .await
        .context("Jira startup check failed")?;
    info!(account = %jira_account, "Jira access OK");
    let slack = SlackClient::new(slack::SLACK_API_URL, &config.slack.bot_token)?;
    let slack_account = slack
        .verify_access()
        .await
        .context("Slack startup check failed")?;
    info!(account = %slack_account, "Slack access OK");
    let actor = SvabActor::new(rx, jira, slack, &config.slack);
    let mut actor_task = tokio::spawn(actor.run());

    let app = server::router(AppState::new(tx, &config));
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("failed to bind {}", config.bind_addr))?;
    info!(addr = %config.bind_addr, "listening");

    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

    // Run until the server shuts down. If the actor dies (e.g. it panicked),
    // the bot can't do anything useful, so exit and let Kubernetes restart it.
    tokio::select! {
        result = server => result.context("HTTP server failed")?,
        result = &mut actor_task => bail!("actor task exited unexpectedly: {result:?}"),
    }

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
