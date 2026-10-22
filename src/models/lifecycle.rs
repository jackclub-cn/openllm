use super::*;

/// Version of the exported configuration document.
///
/// Bumping it means an older gateway must refuse the file rather than silently
/// ignore fields it does not understand; the importer accepts anything up to
/// this number.
pub const CONFIG_DOCUMENT_VERSION: i64 = 1;

/// A portable description of everything an operator configures by hand.
///
/// Deliberately excluded: gateway API keys (only their hash is stored, so they
/// cannot be restored), usage and audit history, and environment-derived
/// runtime limits (they come from the process environment, not the database).
#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigDocument {
    pub openllm_config_version: i64,
    pub exported_at: String,
    pub gateway_version: String,
    /// Whether upstream credentials are in this file.
    pub secrets_included: bool,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub settings: ConfigSettings,
    #[serde(default)]
    pub providers: Vec<ConfigProvider>,
    #[serde(default)]
    pub routes: Vec<ConfigRoute>,
    #[serde(default)]
    pub provider_pools: Vec<ConfigPool>,
    /// Read legacy documents without restoring the retired integration.
    #[serde(default, rename = "webhooks", skip_serializing)]
    pub retired_webhooks: Vec<RetiredWebhook>,
    /// Informational only. Gateway keys cannot be imported because the gateway
    /// stores a hash, never the key itself.
    #[serde(default)]
    pub gateway_keys: Vec<ConfigGatewayKey>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ConfigSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardrails: Option<GuardrailSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspector: Option<InspectorSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resilience: Option<ResilienceSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_retention_days: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigProvider {
    pub name: String,
    pub provider_type: String,
    pub base_url: String,
    #[serde(default)]
    pub model_prefix: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_sync_models: bool,
    /// Models an operator or a sync left enabled.
    #[serde(default)]
    pub models: Vec<String>,
    /// Models kept in the catalog but disabled; without these a round-trip
    /// would silently re-enable everything.
    #[serde(default)]
    pub disabled_models: Vec<String>,
    #[serde(default)]
    pub model_auto_include_rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check_interval_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_sync_interval_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_cooldown_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_timeout_seconds: Option<i64>,
    /// Extra upstream headers. Masked (and therefore never imported) unless
    /// the export included secrets.
    #[serde(default)]
    pub headers: serde_json::Value,
    #[serde(default)]
    pub api_key_set: bool,
    /// Legacy single credential; only present when secrets were exported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default)]
    pub provider_keys: Vec<ConfigProviderKey>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigProviderKey {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_per_minute: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_wait_ms: Option<i64>,
    /// Only present when secrets were exported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigRoute {
    pub name: String,
    pub model_pattern: String,
    pub strategy: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_override: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_override: Option<i64>,
    #[serde(default)]
    pub all_targets_busy_wait_seconds: i64,
    #[serde(default = "default_retry_jitter_ms")]
    pub retry_jitter_ms: i64,
    #[serde(default = "default_max_waiting_requests")]
    pub max_waiting_requests: i64,
    #[serde(default = "default_true")]
    pub keepalive_during_wait: bool,
    #[serde(default)]
    pub targets: Vec<ConfigRouteTarget>,
}

fn default_retry_jitter_ms() -> i64 {
    250
}

fn default_max_waiting_requests() -> i64 {
    32
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigRouteTarget {
    /// Providers are referenced by name so the document survives an import
    /// into a database where the ids differ.
    pub provider: String,
    pub upstream_model: String,
    #[serde(default = "default_weight")]
    pub weight: i64,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigPool {
    pub provider: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_pool_window_seconds")]
    pub window_seconds: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_limit: Option<i64>,
    #[serde(default = "default_pool_saturation_threshold_pct")]
    pub saturation_threshold_pct: i64,
    #[serde(default)]
    pub allocations: Vec<ConfigPoolAllocation>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigPoolAllocation {
    /// Gateway key name. An allocation whose key does not exist is reported
    /// and skipped instead of failing the whole import.
    pub api_key: String,
    #[serde(default = "default_pool_allocation_weight_pct")]
    pub weight_pct: i64,
    #[serde(default = "default_pool_allocation_policy")]
    pub policy: String,
}

#[derive(Debug, Deserialize)]
pub struct RetiredWebhook {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigGatewayKey {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub allowed_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_token_limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_cost_limit_micros: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_per_minute: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExportConfigQuery {
    #[serde(default)]
    pub include_secrets: bool,
}

#[derive(Debug, Deserialize)]
pub struct ImportConfigRequest {
    pub document: ConfigDocument,
    /// `false` (the default) validates and reports without touching the
    /// database, so an operator can review the plan first.
    #[serde(default)]
    pub apply: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct ImportCounts {
    pub create: usize,
    pub update: usize,
    pub unchanged: usize,
    pub skip: usize,
    pub error: usize,
}

#[derive(Debug, Serialize)]
pub struct ImportItem {
    pub kind: &'static str,
    pub name: String,
    /// `create`, `update`, `unchanged`, `skip`, or `error`.
    pub action: &'static str,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct ImportPlan {
    pub dry_run: bool,
    pub applied: bool,
    pub valid: bool,
    pub document_version: i64,
    pub secrets_included: bool,
    pub counts: ImportCounts,
    pub items: Vec<ImportItem>,
    pub problems: Vec<String>,
    /// Caveats the operator should know about an applied import (per-key
    /// identity, the fact that items are written one by one).
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct BackupInfo {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub created_at: String,
    /// `manual` for an on-demand backup, `pre-migration` for the automatic
    /// copy taken before a schema upgrade.
    pub kind: &'static str,
}

#[derive(Debug, Deserialize)]
pub struct VerifyBackupRequest {
    /// File name inside the backups directory. Path separators are rejected.
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct DatabaseFileReport {
    pub file: String,
    pub ok: bool,
    pub size_bytes: u64,
    pub integrity: String,
    pub tables: Vec<String>,
    pub missing_tables: Vec<String>,
    pub applied_migrations: i64,
    pub schema_version: i64,
    pub providers: i64,
    pub routes: i64,
    pub api_keys: i64,
    pub provider_pools: i64,
    pub problems: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct MigrationEntry {
    pub version: i64,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct MigrationReport {
    pub database: String,
    pub up_to_date: bool,
    pub applied: Vec<MigrationEntry>,
    pub pending: Vec<MigrationEntry>,
    /// Migrations recorded in the database that this binary does not know.
    /// A non-empty list means the database was written by a newer build and
    /// this process is a downgrade.
    pub unknown_applied: Vec<i64>,
    pub data_present: bool,
    /// Whether starting the gateway now would first write a backup.
    pub backup_would_be_written: bool,
    pub problems: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpgradeCheckQuery {
    /// Compare against this version instead of fetching a release feed. Lets an
    /// air-gapped operator run the check with no outbound network access.
    #[serde(default)]
    pub latest: Option<String>,
    /// Opt into the project's stable GitHub feed when no custom feed is set.
    #[serde(default)]
    pub official: bool,
}

#[derive(Debug, Serialize)]
pub struct UpgradeStatus {
    pub current_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    pub update_available: bool,
    /// `up_to_date`, `update_available`, `ahead`, or `unknown`.
    pub status: &'static str,
    /// `query`, `feed`, or `none`.
    pub source: &'static str,
    pub checked_at: String,
    pub message: String,
    /// Migrations this build still has to run, reported alongside the version
    /// comparison because an upgrade is a schema change too.
    pub pending_migrations: i64,
}
