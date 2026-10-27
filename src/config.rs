//! Runtime configuration, read from environment variables.
//!
//! The config grows with each milestone: only what the code actually uses is
//! required, so the bot can be run locally with a minimal environment.
//!
//! All variables are prefixed with `SVAB_BOT_` so they can't collide with
//! anything else in the environment. Names are written out in full (rather than
//! built from a prefix constant) so they are easy to grep for.

use std::fmt;
use std::net::SocketAddr;

use anyhow::{Context, bail};

/// Shortest webhook secret we accept. Generate one with `openssl rand -hex 32`.
const MIN_SECRET_LEN: usize = 32;

#[derive(Clone)]
pub struct Config {
    /// Address the HTTP server listens on (`SVAB_BOT_BIND_ADDR`, default `0.0.0.0:8080`).
    pub bind_addr: SocketAddr,
    /// Shared secret that Jira Automation sends as `Authorization: Bearer <secret>`
    /// (`SVAB_BOT_WEBHOOK_SECRET`, required).
    pub webhook_secret: String,
    /// Jira project key that webhook issue keys must belong to
    /// (`SVAB_BOT_JIRA_PROJECT`, default `SVAB`).
    pub jira_project: String,
}

impl Config {
    /// Load the config from the process environment.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Load the config using `lookup` to fetch variables. Split out from
    /// [`Config::from_env`] so tests don't have to modify the real environment.
    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let bind_addr = lookup("SVAB_BOT_BIND_ADDR")
            .unwrap_or_else(|| "0.0.0.0:8080".to_string())
            .parse()
            .context("SVAB_BOT_BIND_ADDR is not a valid socket address")?;

        let webhook_secret =
            lookup("SVAB_BOT_WEBHOOK_SECRET").context("SVAB_BOT_WEBHOOK_SECRET is not set")?;
        if webhook_secret.len() < MIN_SECRET_LEN {
            bail!("SVAB_BOT_WEBHOOK_SECRET must be at least {MIN_SECRET_LEN} characters");
        }

        let jira_project = lookup("SVAB_BOT_JIRA_PROJECT").unwrap_or_else(|| "SVAB".to_string());

        Ok(Self {
            bind_addr,
            webhook_secret,
            jira_project,
        })
    }
}

/// Hand-written so secrets never end up in logs.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("webhook_secret", &"<redacted>")
            .field("jira_project", &self.jira_project)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn load(vars: &[(&str, &str)]) -> anyhow::Result<Config> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|name| vars.get(name).cloned())
    }

    #[test]
    fn defaults_apply() {
        let config = load(&[("SVAB_BOT_WEBHOOK_SECRET", SECRET)]).unwrap();
        assert_eq!(config.bind_addr.to_string(), "0.0.0.0:8080");
        assert_eq!(config.jira_project, "SVAB");
    }

    #[test]
    fn secret_is_required_and_must_be_long() {
        assert!(load(&[]).is_err());
        assert!(load(&[("SVAB_BOT_WEBHOOK_SECRET", "short")]).is_err());
    }

    #[test]
    fn unprefixed_names_are_ignored() {
        assert!(load(&[("WEBHOOK_SECRET", SECRET)]).is_err());
    }

    #[test]
    fn debug_redacts_secret() {
        let config = load(&[("SVAB_BOT_WEBHOOK_SECRET", SECRET)]).unwrap();
        assert!(!format!("{config:?}").contains(SECRET));
    }
}
