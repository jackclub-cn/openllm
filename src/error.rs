use std::time::Duration;

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    TooManyRequests(String),
    #[error("{0}")]
    Upstream(String),
    /// A retryable upstream failure whose HTTP status is meaningful to the
    /// client. Today this carries provider rate limits so a `429` (and its
    /// `Retry-After`) survives all the way back instead of turning into an
    /// opaque `502`.
    #[error("{message}")]
    UpstreamStatus {
        status: StatusCode,
        message: String,
        retry_after: Option<Duration>,
    },
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let retry_after = match &self {
            Self::UpstreamStatus { retry_after, .. } => *retry_after,
            _ => None,
        };
        let (status, error_type) = match &self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request_error"),
            Self::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "authentication_error"),
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "permission_error"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found_error"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict_error"),
            Self::TooManyRequests(_) => (StatusCode::TOO_MANY_REQUESTS, "rate_limit_error"),
            Self::Upstream(_) => (StatusCode::BAD_GATEWAY, "upstream_error"),
            Self::UpstreamStatus { status, .. } => (
                *status,
                if *status == StatusCode::TOO_MANY_REQUESTS {
                    "rate_limit_error"
                } else {
                    "upstream_error"
                },
            ),
            Self::Database(_) | Self::Http(_) | Self::Internal(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
        };

        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        }

        let mut response = (
            status,
            Json(json!({
                "error": {
                    "message": self.to_string(),
                    "type": error_type,
                    "code": status.as_u16()
                }
            })),
        )
            .into_response();
        if let Some(retry_after) = retry_after {
            // Round up so a sub-second window is never reported as "0 seconds",
            // which some clients treat as "retry immediately".
            let seconds = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
            if let Ok(value) = HeaderValue::from_str(&seconds.max(1).to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

pub type AppResult<T> = Result<T, AppError>;
