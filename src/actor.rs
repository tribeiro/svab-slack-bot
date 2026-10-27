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

use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

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
}

impl SvabActor {
    pub fn new(rx: mpsc::Receiver<Command>) -> Self {
        Self { rx }
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

    async fn handle_issue(&mut self, key: &str) -> anyhow::Result<()> {
        // Milestone 1: just prove the plumbing works.
        info!(%key, "issue changed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn replies_and_stops_when_senders_dropped() {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        let actor_task = tokio::spawn(SvabActor::new(rx).run());

        let (reply, reply_rx) = oneshot::channel();
        tx.send(Command::IssueChanged {
            key: "SVAB-1".into(),
            reply,
        })
        .await
        .unwrap();
        assert!(reply_rx.await.unwrap().is_ok());

        // Dropping the last sender ends the actor's loop.
        drop(tx);
        actor_task.await.unwrap();
    }
}
