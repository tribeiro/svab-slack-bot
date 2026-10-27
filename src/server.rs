//! HTTP layer: authenticates incoming webhooks and hands them to the actor.
//!
//! This module does no SVAB logic. Its only jobs are to reject anything that
//! isn't a well-formed, authenticated request, and to translate the actor's
//! answer into an HTTP status code.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{get, post};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, oneshot};

use crate::actor::Command;
use crate::config::Config;
use crate::error::WebhookError;

/// Our payload is tiny; refuse anything bigger than this.
const MAX_BODY_BYTES: usize = 4 * 1024;

/// Shared by all request handlers. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    tx: mpsc::Sender<Command>,
    webhook_secret: Arc<str>,
    jira_project: Arc<str>,
}

impl AppState {
    pub fn new(tx: mpsc::Sender<Command>, config: &Config) -> Self {
        Self {
            tx,
            webhook_secret: config.webhook_secret.as_str().into(),
            jira_project: config.jira_project.as_str().into(),
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/jira/webhook", post(jira_webhook))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

/// Body sent by the Jira Automation rule: `{"issue_key": "{{issue.key}}"}`.
#[derive(Debug, Deserialize)]
struct JiraWebhook {
    issue_key: String,
}

async fn jira_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, WebhookError> {
    // Authenticate before looking at the body at all. We take the body as raw
    // bytes (instead of axum's `Json` extractor) so that unauthenticated
    // callers always get 401, never a parsing error.
    check_bearer(&headers, &state.webhook_secret)?;

    let payload: JiraWebhook = serde_json::from_slice(&body)
        .map_err(|err| WebhookError::BadRequest(format!("invalid JSON body: {err}")))?;
    validate_issue_key(&payload.issue_key, &state.jira_project)?;

    let (reply_tx, reply_rx) = oneshot::channel();
    let command = Command::IssueChanged {
        key: payload.issue_key,
        reply: reply_tx,
    };
    state.tx.try_send(command).map_err(|err| match err {
        mpsc::error::TrySendError::Full(_) => WebhookError::QueueFull,
        mpsc::error::TrySendError::Closed(_) => WebhookError::ActorGone,
    })?;

    // If the actor drops `reply_tx` without answering, it has died.
    reply_rx
        .await
        .map_err(|_| WebhookError::ActorGone)?
        .map_err(WebhookError::Handling)?;

    Ok(StatusCode::OK)
}

/// Check `Authorization: Bearer <secret>` in constant time, so response timing
/// doesn't reveal how much of a guessed secret was right.
fn check_bearer(headers: &HeaderMap, secret: &str) -> Result<(), WebhookError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(WebhookError::Unauthorized)?;

    if bool::from(token.as_bytes().ct_eq(secret.as_bytes())) {
        Ok(())
    } else {
        Err(WebhookError::Unauthorized)
    }
}

/// Accept only keys like `SVAB-123`. The key is later put into Jira API URLs,
/// so this also stops anything like path traversal from getting that far.
fn validate_issue_key(key: &str, project: &str) -> Result<(), WebhookError> {
    let is_valid = key
        .strip_prefix(project)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()));

    if is_valid {
        Ok(())
    } else {
        Err(WebhookError::BadRequest(format!(
            "issue_key must look like {project}-123, got {key:?}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::SvabActor;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn state(tx: mpsc::Sender<Command>) -> AppState {
        AppState {
            tx,
            webhook_secret: SECRET.into(),
            jira_project: "SVAB".into(),
        }
    }

    /// A router backed by a real, running actor.
    fn app() -> Router {
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(SvabActor::new(rx).run());
        router(state(tx))
    }

    fn webhook(auth: Option<&str>, body: &str) -> Request<Body> {
        let mut request = Request::post("/jira/webhook");
        if let Some(auth) = auth {
            request = request.header(header::AUTHORIZATION, auth);
        }
        request.body(Body::from(body.to_string())).unwrap()
    }

    async fn status(app: Router, request: Request<Body>) -> StatusCode {
        app.oneshot(request).await.unwrap().status()
    }

    const GOOD_BODY: &str = r#"{"issue_key": "SVAB-42"}"#;

    #[tokio::test]
    async fn healthz_is_ok() {
        let request = Request::get("/healthz").body(Body::empty()).unwrap();
        assert_eq!(status(app(), request).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn accepts_authenticated_webhook() {
        let auth = format!("Bearer {SECRET}");
        let response = status(app(), webhook(Some(&auth), GOOD_BODY)).await;
        assert_eq!(response, StatusCode::OK);
    }

    #[tokio::test]
    async fn rejects_missing_or_wrong_secret() {
        assert_eq!(
            status(app(), webhook(None, GOOD_BODY)).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(app(), webhook(Some("Bearer nope"), GOOD_BODY)).await,
            StatusCode::UNAUTHORIZED
        );
        // Unauthenticated garbage gets 401 too, not a parsing error.
        assert_eq!(
            status(app(), webhook(None, "not json")).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn rejects_bad_payloads() {
        let auth = format!("Bearer {SECRET}");
        for body in [
            "not json",
            r#"{"key": "SVAB-1"}"#,
            r#"{"issue_key": "OTHER-1"}"#,
            r#"{"issue_key": "SVAB-"}"#,
            r#"{"issue_key": "SVAB-1/../../x"}"#,
        ] {
            let response = status(app(), webhook(Some(&auth), body)).await;
            assert_eq!(response, StatusCode::BAD_REQUEST, "body: {body}");
        }
    }

    #[tokio::test]
    async fn queue_full_returns_503() {
        // Capacity 1, nobody consuming, and the slot already taken.
        let (tx, _rx) = mpsc::channel(1);
        let (reply, _) = oneshot::channel();
        tx.try_send(Command::IssueChanged {
            key: "SVAB-1".into(),
            reply,
        })
        .unwrap();

        let auth = format!("Bearer {SECRET}");
        let response = status(router(state(tx)), webhook(Some(&auth), GOOD_BODY)).await;
        assert_eq!(response, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn dead_actor_returns_500() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);

        let auth = format!("Bearer {SECRET}");
        let response = status(router(state(tx)), webhook(Some(&auth), GOOD_BODY)).await;
        assert_eq!(response, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
