use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Timelike, Utc};
use futures_util::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{QueryBuilder, Row, Sqlite};
use tokio::io::AsyncReadExt;

use crate::error::{AppError, AppResult};
use crate::models::*;
use crate::models_dev;
use crate::proxy::{apply_custom_headers, join_upstream_url, upstream_rejects_tool_search};
use crate::state::{AppState, SETTING_GUARDRAILS, SETTING_INSPECTOR};

mod admin;
mod audit;
mod helpers;
mod keys;
mod metrics;
mod models;
mod providers;
mod routes;
mod usage;
mod webhooks;

pub use admin::*;
pub use audit::*;
use helpers::*;
pub use keys::*;
pub use metrics::*;
pub use models::*;
pub(crate) use providers::*;
pub use routes::*;
pub use usage::*;
pub use webhooks::*;

#[cfg(test)]
#[path = "../tests/unit/api.rs"]
mod tests;

const SETTING_USAGE_RETENTION_DAYS: &str = "usage_retention_days";
const USAGE_RETENTION_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);
const MAX_OVERVIEW_RANGE_DAYS: i64 = 366;
