//! Minimal Slack Web API client: just `chat.postMessage`.
//!
//! Slack returns HTTP 200 even for failures; the real result is the `ok` field
//! in the JSON body, with the reason in `error` (e.g. `channel_not_found`).

use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

/// Production Slack API base URL. Tests point the client at a fake server.
pub const SLACK_API_URL: &str = "https://slack.com/api";

/// Don't let a slow Slack block the actor forever.
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct SlackClient {
    http: reqwest::Client,
    api_url: String,
    bot_token: String,
}

impl SlackClient {
    pub fn new(api_url: &str, bot_token: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder().timeout(TIMEOUT).build()?;
        Ok(Self {
            http,
            api_url: api_url.trim_end_matches('/').to_string(),
            bot_token: bot_token.to_string(),
        })
    }

    /// Post a message, as a thread reply if `thread_ts` is given.
    /// Returns the new message's `ts`, which identifies it within the channel.
    pub async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> anyhow::Result<String> {
        let request = PostMessage {
            channel,
            text,
            thread_ts,
            unfurl_links: false,
        };
        let response: PostMessageResponse = self
            .http
            .post(format!("{}/chat.postMessage", self.api_url))
            .bearer_auth(&self.bot_token)
            .json(&request)
            .send()
            .await
            .context("chat.postMessage")?
            .error_for_status()
            .context("chat.postMessage")?
            .json()
            .await
            .context("parsing chat.postMessage response")?;

        match response {
            PostMessageResponse {
                ok: true,
                ts: Some(ts),
                ..
            } => Ok(ts),
            PostMessageResponse { error, .. } => bail!(
                "chat.postMessage failed: {}",
                error.as_deref().unwrap_or("unknown error")
            ),
        }
    }
}

#[derive(Serialize)]
struct PostMessage<'a> {
    channel: &'a str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_ts: Option<&'a str>,
    /// Don't expand the Jira link into a big preview card.
    unfurl_links: bool,
}

#[derive(Deserialize)]
struct PostMessageResponse {
    ok: bool,
    ts: Option<String>,
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn posts_thread_reply_and_returns_ts() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat.postMessage"))
            .and(header("authorization", "Bearer xoxb-test"))
            .and(body_partial_json(json!({
                "channel": "C123",
                "text": "hello",
                "thread_ts": "111.222",
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"ok": true, "ts": "333.444"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let slack = SlackClient::new(&server.uri(), "xoxb-test").unwrap();
        let ts = slack.post_message("C123", "hello", Some("111.222")).await;
        assert_eq!(ts.unwrap(), "333.444");
    }

    #[tokio::test]
    async fn slack_errors_are_reported() {
        let server = MockServer::start().await;
        Mock::given(path("/chat.postMessage"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"ok": false, "error": "channel_not_found"})),
            )
            .mount(&server)
            .await;

        let slack = SlackClient::new(&server.uri(), "xoxb-test").unwrap();
        let err = slack.post_message("C123", "hello", None).await.unwrap_err();
        assert!(err.to_string().contains("channel_not_found"));
    }
}
