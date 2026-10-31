//! Reconciler: catches up on Jira events the bot missed.
//!
//! Jira Automation doesn't retry a failed web request, so anything that
//! happens while the bot is down (or a webhook that fails) would be lost. The
//! reconciler runs once at startup and then every interval: it asks Jira for
//! recently updated SVAB tickets and sends each one to the actor as an
//! ordinary [`Command::IssueChanged`], exactly like the webhook does.
//!
//! No catch-up logic lives here. `svab::decide` only acts when something is
//! actually new, so a ticket that was already handled costs two Jira reads and
//! posts nothing.

use tokio::sync::{mpsc, oneshot};
use tokio::time::{self, MissedTickBehavior};
use tracing::{info, warn};

use crate::actor::Command;
use crate::config::ReconcileConfig;
use crate::jira::JiraClient;

pub struct Reconciler {
    jira: JiraClient,
    tx: mpsc::Sender<Command>,
    project: String,
    config: ReconcileConfig,
}

/// Outcome of one reconcile pass.
#[derive(Debug, PartialEq, Eq)]
enum Pass {
    /// Every ticket found was sent to the actor; `failed` of them errored
    /// (the actor already logged why).
    Done { checked: usize, failed: usize },
    /// Couldn't ask Jira for tickets; try again next pass.
    SearchFailed,
    /// The actor is gone, so there's no point continuing.
    ActorStopped,
}

impl Reconciler {
    pub fn new(
        jira: JiraClient,
        tx: mpsc::Sender<Command>,
        project: &str,
        config: ReconcileConfig,
    ) -> Self {
        Self {
            jira,
            tx,
            project: project.to_string(),
            config,
        }
    }

    /// Run a pass now, then one every interval, until the actor stops.
    pub async fn run(self) {
        let mut ticker = time::interval(self.config.interval);
        // If a pass overruns the interval, start the next one a full interval
        // later instead of firing a burst of catch-up passes.
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            // The first tick completes immediately: that's the startup pass.
            ticker.tick().await;
            match self.reconcile_once().await {
                Pass::Done { checked, failed } => {
                    info!(checked, failed, "reconcile pass done");
                }
                Pass::SearchFailed => {}
                Pass::ActorStopped => {
                    info!("actor stopped; reconciler stopping");
                    return;
                }
            }
        }
    }

    async fn reconcile_once(&self) -> Pass {
        let keys = match self
            .jira
            .search_recently_updated(&self.project, self.config.lookback_hours)
            .await
        {
            Ok(keys) => keys,
            Err(err) => {
                warn!("reconcile: Jira search failed, will retry next pass: {err:#}");
                return Pass::SearchFailed;
            }
        };

        let mut failed = 0;
        for key in &keys {
            let (reply, reply_rx) = oneshot::channel();
            let command = Command::IssueChanged {
                key: key.clone(),
                reply,
            };
            // Unlike the webhook, nobody is waiting on us, so if the queue is
            // full we wait for room instead of giving up.
            if self.tx.send(command).await.is_err() {
                return Pass::ActorStopped;
            }
            // Wait for each ticket before sending the next, so a pass never
            // fills the queue and webhooks get their turn in between.
            match reply_rx.await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => failed += 1,
                Err(_) => return Pass::ActorStopped,
            }
        }

        Pass::Done {
            checked: keys.len(),
            failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::JiraConfig;
    use serde_json::json;
    use std::time::Duration;
    use tokio::task::JoinHandle;
    use wiremock::matchers::path;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn reconciler(server: &MockServer, tx: mpsc::Sender<Command>) -> Reconciler {
        let jira = JiraClient::new(JiraConfig {
            site_url: "https://rubinobs.atlassian.net".into(),
            api_url: server.uri(),
            email: "bot@example.com".into(),
            api_token: "token".into(),
        })
        .unwrap();
        let config = ReconcileConfig {
            interval: Duration::from_secs(15 * 60),
            lookback_hours: 72,
        };
        Reconciler::new(jira, tx, "SVAB", config)
    }

    async fn mount_search(server: &MockServer, keys: &[&str]) {
        let issues: Vec<_> = keys.iter().map(|key| json!({"key": key})).collect();
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"issues": issues, "isLast": true})),
            )
            .mount(server)
            .await;
    }

    /// Stand-in actor: records the keys it receives and fails the ones in
    /// `failing`. Returns the keys once all senders are dropped.
    fn fake_actor(
        mut rx: mpsc::Receiver<Command>,
        failing: &'static [&'static str],
    ) -> JoinHandle<Vec<String>> {
        tokio::spawn(async move {
            let mut seen = Vec::new();
            while let Some(Command::IssueChanged { key, reply }) = rx.recv().await {
                let result = if failing.contains(&key.as_str()) {
                    Err(anyhow::anyhow!("boom"))
                } else {
                    Ok(())
                };
                seen.push(key);
                let _ = reply.send(result);
            }
            seen
        })
    }

    #[tokio::test]
    async fn sends_every_ticket_and_counts_failures() {
        let server = MockServer::start().await;
        mount_search(&server, &["SVAB-1", "SVAB-2", "SVAB-3"]).await;
        let (tx, rx) = mpsc::channel(1);
        let actor = fake_actor(rx, &["SVAB-2"]);

        let pass = reconciler(&server, tx).reconcile_once().await;
        assert_eq!(
            pass,
            Pass::Done {
                checked: 3,
                failed: 1
            }
        );
        // The reconciler (and its sender) is dropped, so the actor finishes.
        assert_eq!(actor.await.unwrap(), ["SVAB-1", "SVAB-2", "SVAB-3"]);
    }

    #[tokio::test]
    async fn search_failure_sends_nothing() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/api/3/search/jql"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let (tx, rx) = mpsc::channel(1);
        let actor = fake_actor(rx, &[]);

        let pass = reconciler(&server, tx).reconcile_once().await;
        assert_eq!(pass, Pass::SearchFailed);
        assert!(actor.await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_stops_when_actor_is_gone() {
        let server = MockServer::start().await;
        mount_search(&server, &["SVAB-1"]).await;
        let (tx, rx) = mpsc::channel(1);
        drop(rx);

        // Returns from the startup pass instead of looping forever.
        time::timeout(Duration::from_secs(5), reconciler(&server, tx).run())
            .await
            .expect("reconciler should stop when the actor is gone");
    }
}
