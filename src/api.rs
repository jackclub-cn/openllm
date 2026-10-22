use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Datelike, Duration, Months, Timelike, Utc};
use futures_util::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Row, Sqlite};
use tokio::io::AsyncReadExt;

use crate::error::{AppError, AppResult};
use crate::models::*;
use crate::models_dev;
use crate::proxy::{
    apply_custom_headers, apply_opencode_provider_session_header, join_upstream_url,
    upstream_rejects_tool_search,
};
use crate::state::{AppState, SETTING_GUARDRAILS, SETTING_INSPECTOR, SETTING_RESILIENCE};

mod activity;
mod admin;
mod audit;
pub(crate) mod diagnostics;
mod helpers;
mod keys;
pub(crate) mod lifecycle;
mod metrics;
mod models;
mod pools;
mod providers;
mod resilience;
mod routes;
mod usage;

pub use activity::*;
pub use admin::*;
pub use audit::*;
pub(crate) use diagnostics::*;
use helpers::*;
pub use keys::*;
pub(crate) use lifecycle::*;
pub use metrics::*;
pub use models::*;
pub(crate) use pools::*;
pub(crate) use providers::*;
pub use resilience::*;
pub use routes::*;
pub use usage::*;

#[cfg(test)]
#[path = "../tests/unit/api.rs"]
mod tests;

const SETTING_USAGE_RETENTION_DAYS: &str = "usage_retention_days";
const USAGE_RETENTION_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);
const MAX_OVERVIEW_RANGE_DAYS: i64 = 366;
const MAX_HOURLY_OVERVIEW_DAYS: i64 = 31;
