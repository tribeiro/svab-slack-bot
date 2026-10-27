//! SVAB Slack bot: posts SVAB ticket updates to Slack, using threads.

mod config;

use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::config::Config;

fn main() -> anyhow::Result<()> {
    // Log level comes from RUST_LOG (e.g. `RUST_LOG=debug`), default `info`.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    info!(?config, "starting svab-slack-bot");
    Ok(())
}
