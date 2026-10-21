use std::collections::{HashMap, HashSet};
use std::io;
use std::str::FromStr;
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use chrono::{Timelike, Utc};
use futures_util::StreamExt;
use globset::Glob;
use rand::distributions::{Distribution, WeightedIndex};
use reqwest::RequestBuilder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tokio_stream::wrappers::ReceiverStream;

use crate::error::{AppError, AppResult};
use crate::models::{
    ApiKeyRecord, ModelCapabilities, ModelList, ProviderType, PublicModel, Route,
    RouteDiagnoseCandidate, RouteDiagnoseFactor, RouteDiagnoseRuntimeTarget, RouteDiagnoseTarget,
    RouteDiagnoseView, RouteStrategy, RouteTarget, Usage, effective_cost_value,
    estimate_cost_micros,
};
use crate::registry::BarrelEnvelope;
use crate::state::AppState;

mod activity;
mod anthropic_api;
mod auth;
mod compat;
mod failure;
mod guardrails;
mod models_api;
mod openai_api;
mod route_trace;
mod routing;
mod session;
mod translation;
mod upstream;
mod usage;

use activity::*;
pub(crate) use anthropic_api::*;
pub(crate) use auth::*;
pub(crate) use compat::*;
use failure::*;
use guardrails::*;
pub(crate) use models_api::*;
pub(crate) use openai_api::*;
use routing::*;
pub(crate) use routing::{clear_target_sinks_for_route, diagnose_route, diagnose_route_for_key};
pub(crate) use session::*;
use translation::*;
pub(crate) use upstream::*;
pub(crate) use usage::*;

#[cfg(test)]
#[path = "../tests/unit/proxy.rs"]
mod tests;

const OPENAI_CHAT_COMPLETIONS: &str = "/v1/chat/completions";
const OPENAI_COMPLETIONS: &str = "/v1/completions";
const CONSOLE_API_KEY_ID_HEADER: &str = "x-openllm-api-key-id";
const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";
const SESSION_ID_MAX_CHARS: usize = 256;
const OPENAI_RESPONSES: &str = "/v1/responses";
const MAX_UPSTREAM_RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
const MAX_PROVIDER_COOLDOWN: Duration = Duration::from_secs(5 * 60);
const MAX_TARGET_COOLDOWN: Duration = Duration::from_secs(5 * 60);
const MAX_CONFIGURED_PROVIDER_COOLDOWN: Duration = Duration::from_secs(60 * 60);
const DEFAULT_PROVIDER_QUEUE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_SAME_TARGET_RETRIES: i64 = 5;
const MAX_PROVIDER_KEY_COOLDOWN: Duration = Duration::from_secs(60 * 60);
const PROVIDER_MODEL_COOLDOWN_THRESHOLD: usize = 2;

#[derive(Debug)]
pub(super) enum AcquireSlotError {
    Provider(AppError),
    Model(AppError),
    ProviderApiKey(AppError),
}

impl AcquireSlotError {
    pub(super) fn provider_exhausted(&self) -> bool {
        matches!(self, Self::Provider(_))
    }

    pub(super) fn into_error(self) -> AppError {
        match self {
            Self::Provider(error) | Self::Model(error) | Self::ProviderApiKey(error) => error,
        }
    }
}

/// Permits held until the upstream response body has been fully consumed.
#[derive(Debug)]
pub(super) struct UpstreamSlots {
    pub(super) provider_key: Option<OwnedSemaphorePermit>,
    pub(super) provider: Option<OwnedSemaphorePermit>,
    pub(super) model: Option<OwnedSemaphorePermit>,
}

#[derive(Debug, Clone)]
struct ResolvedRoute {
    route_id: Option<i64>,
    route_name: Option<String>,
    strategy: String,
    targets: Vec<RouteTarget>,
    /// Strictest common capability envelope across the route's targets. Only
    /// meaningful for explicit routes; a directly matched model reports its own
    /// capabilities.
    barrel: Option<BarrelEnvelope>,
    /// Route-level bounded patience. Zero keeps each target single-shot.
    all_targets_busy_wait_seconds: i64,
    retry_jitter_ms: i64,
    max_waiting_requests: i64,
    keepalive_during_wait: bool,
}
