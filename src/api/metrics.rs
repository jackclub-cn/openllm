use super::*;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

mod configuration;
mod exposition;
mod gateway;
mod pools;
mod providers;
mod runtime;
mod snapshot;
use exposition::*;
use snapshot::MetricsSnapshot;

pub async fn prometheus_metrics(State(state): State<AppState>) -> AppResult<Response> {
    let snapshot = snapshot::collect(&state).await?;
    let mut body = String::with_capacity(8 * 1024);
    snapshot.append_gateway(&state, &mut body);
    snapshot.append_configuration(&mut body);
    snapshot.append_providers(&mut body);
    snapshot.append_pools(&mut body);
    snapshot.append_runtime(&state, &mut body);

    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .map_err(|error| AppError::Internal(error.into()))
}
