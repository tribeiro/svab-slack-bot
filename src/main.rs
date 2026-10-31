//! SVAB Slack bot: posts SVAB ticket updates to Slack, using threads.
//!
//! See README.md for the architecture. In short:
//! `server` (HTTP) --Command--> `actor` (does the work, one command at a time),
//! which uses `jira` and `slack` clients, `svab::decide` and `messages`.
//! `reconciler` periodically sends the same commands to catch up on missed events.

mod actor;
mod config;
mod error;
mod jira;
mod messages;
mod reconciler;
mod server;
mod slack;
mod svab;

use anyhow::{Context, bail};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::actor::SvabActor;
use crate::config::Config;
use crate::jira::JiraClient;
use crate::reconciler::Reconciler;
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
    if config.announce_since.is_none() {
        warn!(
            "SVAB_BOT_ANNOUNCE_SINCE is not set: tickets already announced by the \
             old Jira plugin may be announced again"
        );
    }
    // The reconciler sends the same commands as the webhook, through its own
    // clone of the sender. JiraClient is cheap to clone (shared connection pool).
    let reconciler = Reconciler::new(
        jira.clone(),
        tx.clone(),
        &config.jira_project,
        config.reconcile.clone(),
    );
    let actor = SvabActor::new(rx, jira, slack, &config.slack, config.announce_since);
    let mut actor_task = tokio::spawn(actor.run());
    let mut reconciler_task = tokio::spawn(reconciler.run());

    let app = server::router(AppState::new(tx, &config));
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("failed to bind {}", config.bind_addr))?;
    info!(addr = %config.bind_addr, "listening");

    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

    // Run until the server shuts down. If the actor or the reconciler dies
    // (e.g. it panicked), exit and let Kubernetes restart the bot: without the
    // actor nothing works, and without the reconciler missed events would go
    // unnoticed.
    tokio::select! {
        result = server => result.context("HTTP server failed")?,
        result = &mut actor_task => bail!("actor task exited unexpectedly: {result:?}"),
        result = &mut reconciler_task => bail!("reconciler task exited unexpectedly: {result:?}"),
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
