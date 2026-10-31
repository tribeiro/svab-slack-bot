//! The SVAB actor: a single task that owns all the work.
//!
//! Event sources (the webhook handler today, the reconciler later) never call
//! Jira or Slack themselves. They send a [`Command`] over an `mpsc` channel and
//! the actor processes commands one at a time, in order. Because only one
//! command runs at a time, two events for the same ticket can never race each
//! other, and no locks are needed.
//!
//! Each command carries a `oneshot` sender so the caller can wait for the
//! outcome (the webhook handler turns it into the HTTP status code).
//!
//! For each ticket the actor gathers the facts (the issue and our stored
//! thread state), asks [`svab::decide`] what to do, and carries out the
//! resulting [`Action`]s.

use anyhow::Context;
use jiff::Timestamp;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::config::SlackConfig;
use crate::jira::JiraClient;
use crate::messages::{self, Mentions};
use crate::slack::SlackClient;
use crate::svab::{self, Action, Issue, ThreadState};

/// How many commands can wait in the queue before senders get "queue full".
pub const QUEUE_CAPACITY: usize = 32;

/// Messages the actor understands. New kinds of work become new variants.
#[derive(Debug)]
pub enum Command {
    /// An SVAB ticket may have changed; look at it and notify Slack if needed.
    IssueChanged {
        key: String,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
}

pub struct SvabActor {
    rx: mpsc::Receiver<Command>,
    jira: JiraClient,
    slack: SlackClient,
    channel_id: String,
    mentions: Mentions,
    announce_since: Option<Timestamp>,
}

impl SvabActor {
    pub fn new(
        rx: mpsc::Receiver<Command>,
        jira: JiraClient,
        slack: SlackClient,
        slack_config: &SlackConfig,
        announce_since: Option<Timestamp>,
    ) -> Self {
        Self {
            rx,
            jira,
            slack,
            announce_since,
            channel_id: slack_config.channel_id.clone(),
            mentions: Mentions {
                approvers_group_id: slack_config.approvers_group_id.clone(),
                main_group_id: slack_config.main_group_id.clone(),
            },
        }
    }

    /// Process commands until every sender has been dropped.
    pub async fn run(mut self) {
        while let Some(command) = self.rx.recv().await {
            match command {
                Command::IssueChanged { key, reply } => {
                    let result = self.handle_issue(&key).await;
                    if let Err(err) = &result {
                        error!(%key, "failed to handle issue: {err:#}");
                    }
                    // The caller may have given up waiting (e.g. the HTTP
                    // client disconnected); that is not an error for us.
                    let _ = reply.send(result);
                }
            }
        }
        info!("all command senders dropped; actor stopping");
    }

    async fn handle_issue(&self, key: &str) -> anyhow::Result<()> {
        // Always read the current state from Jira; never trust the event.
        let issue = self.jira.get_issue(key).await?;
        let thread = self.jira.get_thread_state(key).await?;

        let actions = svab::decide(&issue, thread.as_ref(), self.announce_since);
        debug!(%key, status = issue.status.name(), ?actions, "decided");

        for action in actions {
            match action {
                Action::Announce => self.announce(&issue).await?,
                Action::WarnNeverAnnounced => warn!(
                    %key,
                    status = issue.status.name(),
                    "ticket went through review without being announced \
                     (bot down at the time?); not announcing it late"
                ),
            }
        }
        Ok(())
    }

    /// Post the announcement, save the thread, then post the details in it.
    async fn announce(&self, issue: &Issue) -> anyhow::Result<()> {
        let text = messages::announcement(issue, &self.mentions);
        let ts = self
            .slack
            .post_message(&self.channel_id, &text, None)
            .await
            .context("posting announcement")?;
        info!(key = %issue.key, %ts, "posted announcement");

        // Save the thread before doing anything else, so that if a later step
        // fails, a retry won't post a second announcement.
        let state = ThreadState {
            channel: self.channel_id.clone(),
            thread_ts: ts.clone(),
            last_status: issue.status.clone(),
        };
        self.jira
            .set_thread_state(&issue.key, &state)
            .await
            .with_context(|| {
                format!("announcement posted (ts {ts}) but saving the thread state failed")
            })?;

        self.slack
            .post_message(&self.channel_id, &messages::details(issue), Some(&ts))
            .await
            .context("posting details in the thread")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::JiraConfig;
    use crate::jira::tests::svab_15_json;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PROPERTY_PATH: &str = "/rest/api/3/issue/SVAB-15/properties/svab-slack-bot";

    /// An actor whose Jira and Slack clients both talk to `server`.
    fn actor(server: &MockServer, rx: mpsc::Receiver<Command>) -> SvabActor {
        let jira = JiraClient::new(JiraConfig {
            site_url: "https://rubinobs.atlassian.net".into(),
            api_url: server.uri(),
            email: "bot@example.com".into(),
            api_token: "token".into(),
        })
        .unwrap();
        let slack = SlackClient::new(&server.uri(), "xoxb-test").unwrap();
        let slack_config = SlackConfig {
            bot_token: "xoxb-test".into(),
            channel_id: "C123".into(),
            approvers_group_id: "S111".into(),
            main_group_id: "S222".into(),
        };
        SvabActor::new(rx, jira, slack, &slack_config, None)
    }

    async fn send(tx: &mpsc::Sender<Command>, key: &str) -> oneshot::Receiver<anyhow::Result<()>> {
        let (reply, reply_rx) = oneshot::channel();
        tx.send(Command::IssueChanged {
            key: key.into(),
            reply,
        })
        .await
        .unwrap();
        reply_rx
    }

    #[tokio::test]
    async fn announces_once_even_for_back_to_back_events() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/SVAB-15"))
            .respond_with(ResponseTemplate::new(200).set_body_json(svab_15_json()))
            .mount(&server)
            .await;
        // No thread state the first time we look; after that, what we saved.
        Mock::given(method("GET"))
            .and(path(PROPERTY_PATH))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(PROPERTY_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "key": "svab-slack-bot",
                "value": {"channel": "C123", "thread_ts": "111.000", "last_status": "In Review"}
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path(PROPERTY_PATH))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        // Announcement + details reply, and nothing for the second event.
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"ok": true, "ts": "111.000"})),
            )
            .expect(2)
            .mount(&server)
            .await;

        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        let actor_task = tokio::spawn(actor(&server, rx).run());

        // Queue both events before waiting for either.
        let first = send(&tx, "SVAB-15").await;
        let second = send(&tx, "SVAB-15").await;
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();

        // The details went into the announcement's thread.
        let posts: Vec<serde_json::Value> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/chat.postMessage")
            .map(|request| request.body_json().unwrap())
            .collect();
        assert!(posts[0].get("thread_ts").is_none());
        assert!(
            posts[0]["text"]
                .as_str()
                .unwrap()
                .contains("ready for review")
        );
        assert_eq!(posts[1]["thread_ts"], "111.000");
        assert!(
            posts[1]["text"]
                .as_str()
                .unwrap()
                .starts_with("Details for SVAB-15")
        );

        // Dropping the last sender ends the actor's loop.
        drop(tx);
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn jira_errors_are_replied() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/issue/SVAB-15"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        tokio::spawn(actor(&server, rx).run());

        assert!(send(&tx, "SVAB-15").await.await.unwrap().is_err());
    }
}
