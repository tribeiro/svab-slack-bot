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
    pub jira: JiraConfig,
    pub slack: SlackConfig,
}

/// How to reach Jira, and as whom.
#[derive(Clone)]
pub struct JiraConfig {
    /// Jira site, used for links in messages (`SVAB_BOT_JIRA_SITE_URL`,
    /// required), e.g. `https://rubinobs.atlassian.net`.
    pub site_url: String,
    /// Base URL for REST calls (`SVAB_BOT_JIRA_API_URL`, default: the site URL).
    /// Scoped API tokens (e.g. Atlassian service accounts) must use
    /// `https://api.atlassian.com/ex/jira/<cloud-id>` instead.
    pub api_url: String,
    /// Account the bot authenticates as (`SVAB_BOT_JIRA_EMAIL`, required).
    pub email: String,
    /// API token for that account (`SVAB_BOT_JIRA_API_TOKEN`, required).
    pub api_token: String,
}

/// Where to post in Slack, and as whom.
#[derive(Clone)]
pub struct SlackConfig {
    /// Bot token of the Slack app, `xoxb-...` (`SVAB_BOT_SLACK_BOT_TOKEN`, required).
    pub bot_token: String,
    /// Channel to post announcements in (`SVAB_BOT_SLACK_CHANNEL_ID`, required).
    pub channel_id: String,
    /// User group ID (`S...`) of @svab-approvers (`SVAB_BOT_SLACK_APPROVERS_GROUP_ID`, required).
    pub approvers_group_id: String,
    /// User group ID (`S...`) of @svab-main (`SVAB_BOT_SLACK_MAIN_GROUP_ID`, required).
    pub main_group_id: String,
}

impl Config {
    /// Load the config from the process environment.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Load the config using `lookup` to fetch variables. Split out from
    /// [`Config::from_env`] so tests don't have to modify the real environment.
    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let required = |name: &str| lookup(name).with_context(|| format!("{name} is not set"));

        let bind_addr = lookup("SVAB_BOT_BIND_ADDR")
            .unwrap_or_else(|| "0.0.0.0:8080".to_string())
            .parse()
            .context("SVAB_BOT_BIND_ADDR is not a valid socket address")?;

        let webhook_secret = required("SVAB_BOT_WEBHOOK_SECRET")?;
        if webhook_secret.len() < MIN_SECRET_LEN {
            bail!("SVAB_BOT_WEBHOOK_SECRET must be at least {MIN_SECRET_LEN} characters");
        }

        let jira_project = lookup("SVAB_BOT_JIRA_PROJECT").unwrap_or_else(|| "SVAB".to_string());

        // Trailing slashes would give `//rest/...` URLs.
        let site_url = required("SVAB_BOT_JIRA_SITE_URL")?
            .trim_end_matches('/')
            .to_string();
        let api_url = lookup("SVAB_BOT_JIRA_API_URL")
            .map(|url| url.trim_end_matches('/').to_string())
            .unwrap_or_else(|| site_url.clone());
        let jira = JiraConfig {
            site_url,
            api_url,
            email: required("SVAB_BOT_JIRA_EMAIL")?,
            api_token: required("SVAB_BOT_JIRA_API_TOKEN")?,
        };

        let slack = SlackConfig {
            bot_token: required("SVAB_BOT_SLACK_BOT_TOKEN")?,
            channel_id: required("SVAB_BOT_SLACK_CHANNEL_ID")?,
            approvers_group_id: required("SVAB_BOT_SLACK_APPROVERS_GROUP_ID")?,
            main_group_id: required("SVAB_BOT_SLACK_MAIN_GROUP_ID")?,
        };

        Ok(Self {
            bind_addr,
            webhook_secret,
            jira_project,
            jira,
            slack,
        })
    }
}

// Debug impls are hand-written so secrets never end up in logs.

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("webhook_secret", &"<redacted>")
            .field("jira_project", &self.jira_project)
            .field("jira", &self.jira)
            .field("slack", &self.slack)
            .finish()
    }
}

impl fmt::Debug for JiraConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JiraConfig")
            .field("site_url", &self.site_url)
            .field("api_url", &self.api_url)
            .field("email", &self.email)
            .field("api_token", &"<redacted>")
            .finish()
    }
}

impl fmt::Debug for SlackConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlackConfig")
            .field("bot_token", &"<redacted>")
            .field("channel_id", &self.channel_id)
            .field("approvers_group_id", &self.approvers_group_id)
            .field("main_group_id", &self.main_group_id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    /// All required variables, with recognizable values.
    fn required_vars() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("SVAB_BOT_WEBHOOK_SECRET", SECRET),
            ("SVAB_BOT_JIRA_SITE_URL", "https://example.atlassian.net/"),
            ("SVAB_BOT_JIRA_EMAIL", "bot@example.com"),
            ("SVAB_BOT_JIRA_API_TOKEN", "jira-token-value"),
            ("SVAB_BOT_SLACK_BOT_TOKEN", "xoxb-slack-token-value"),
            ("SVAB_BOT_SLACK_CHANNEL_ID", "C123"),
            ("SVAB_BOT_SLACK_APPROVERS_GROUP_ID", "S111"),
            ("SVAB_BOT_SLACK_MAIN_GROUP_ID", "S222"),
        ])
    }

    fn load(vars: &HashMap<&str, &str>) -> anyhow::Result<Config> {
        Config::from_lookup(|name| vars.get(name).map(|v| v.to_string()))
    }

    #[test]
    fn defaults_apply() {
        let config = load(&required_vars()).unwrap();
        assert_eq!(config.bind_addr.to_string(), "0.0.0.0:8080");
        assert_eq!(config.jira_project, "SVAB");
        // Trailing slash trimmed, and the API URL defaults to the site URL.
        assert_eq!(config.jira.site_url, "https://example.atlassian.net");
        assert_eq!(config.jira.api_url, "https://example.atlassian.net");
    }

    #[test]
    fn api_url_can_be_overridden() {
        let mut vars = required_vars();
        vars.insert(
            "SVAB_BOT_JIRA_API_URL",
            "https://api.atlassian.com/ex/jira/abc",
        );
        let config = load(&vars).unwrap();
        assert_eq!(config.jira.api_url, "https://api.atlassian.com/ex/jira/abc");
    }

    #[test]
    fn every_required_variable_is_required() {
        for name in required_vars().keys() {
            let mut vars = required_vars();
            vars.remove(name);
            let err = load(&vars).unwrap_err();
            assert!(err.to_string().contains(name), "{name}: {err}");
        }
    }

    #[test]
    fn secret_must_be_long() {
        let mut vars = required_vars();
        vars.insert("SVAB_BOT_WEBHOOK_SECRET", "short");
        assert!(load(&vars).is_err());
    }

    #[test]
    fn unprefixed_names_are_ignored() {
        let mut vars = required_vars();
        vars.remove("SVAB_BOT_WEBHOOK_SECRET");
        vars.insert("WEBHOOK_SECRET", SECRET);
        assert!(load(&vars).is_err());
    }

    #[test]
    fn debug_redacts_secrets() {
        let debug = format!("{:?}", load(&required_vars()).unwrap());
        for secret in [SECRET, "jira-token-value", "xoxb-slack-token-value"] {
            assert!(!debug.contains(secret), "{secret} leaked: {debug}");
        }
    }
}
