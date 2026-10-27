//! Errors returned by the HTTP layer, and the status code each one maps to.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    #[error("missing or invalid bearer token")]
    Unauthorized,
    #[error("invalid request: {0}")]
    BadRequest(String),
    #[error("event queue is full, try again later")]
    QueueFull,
    #[error("event handler is not running")]
    ActorGone,
    #[error("failed to handle event: {0:#}")]
    Handling(anyhow::Error),
}

impl IntoResponse for WebhookError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::QueueFull => StatusCode::SERVICE_UNAVAILABLE,
            Self::ActorGone | Self::Handling(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // The message ends up in the Jira Automation audit log, which helps
        // whoever is debugging a failed rule.
        (status, self.to_string()).into_response()
    }
}
