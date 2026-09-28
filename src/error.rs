use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Upstream(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_type) = match &self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "invalid_request_error"),
            Self::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "authentication_error"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found_error"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict_error"),
            Self::Upstream(_) => (StatusCode::BAD_GATEWAY, "upstream_error"),
            Self::Database(_) | Self::Http(_) | Self::Internal(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
        };

        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        }

        (
            status,
            Json(json!({
                "error": {
                    "message": self.to_string(),
                    "type": error_type,
                    "code": status.as_u16()
                }
            })),
        )
            .into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
