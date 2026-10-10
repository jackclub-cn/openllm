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
    RouteDiagnoseRuntimeTarget, RouteDiagnoseTarget, RouteDiagnoseView, RouteStrategy, RouteTarget,
    Usage, effective_cost_value, estimate_cost_micros,
};
use crate::registry::BarrelEnvelope;
use crate::state::AppState;

mod anthropic_api;
mod auth;
mod compat;
mod guardrails;
mod models_api;
mod openai_api;
mod routing;
mod session;
mod translation;
mod upstream;
mod usage;

pub(crate) use anthropic_api::*;
pub(crate) use auth::*;
pub(crate) use compat::*;
use guardrails::*;
pub(crate) use models_api::*;
pub(crate) use openai_api::*;
pub(crate) use routing::diagnose_route;
use routing::*;
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
const MAX_PROVIDER_KEY_COOLDOWN: Duration = Duration::from_secs(60 * 60);
const PROVIDER_RATE_LIMIT_MODEL_THRESHOLD: usize = 2;

#[derive(Debug)]
struct ResolvedRoute {
    route_id: Option<i64>,
    strategy: String,
    targets: Vec<RouteTarget>,
    /// Strictest common capability envelope across the route's targets. Only
    /// meaningful for explicit routes; a directly matched model reports its own
    /// capabilities.
    barrel: Option<BarrelEnvelope>,
}
