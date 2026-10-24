use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

use crate::error::AppResult;
use crate::state::AppState;

pub(crate) const CAPABILITY_TOOL_SEARCH: &str = "tool_search";
pub(crate) const CAPABILITY_REASONING_HISTORY: &str = "reasoning_history";
pub(crate) const CAPABILITY_STRICT_TOOLS: &str = "strict_tools";
pub(crate) const CAPABILITY_IMAGE_DETAIL_ORIGINAL: &str = "image_detail_original";

pub(crate) const SOURCE_RUNTIME: &str = "runtime";
pub(crate) const SOURCE_TEMPLATE: &str = "template";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CapabilityKey {
    pub provider_id: i64,
    pub model: String,
    pub endpoint: String,
    pub capability: String,
}

#[derive(Clone, Debug)]
pub(crate) struct CapabilityRecord {
    pub key: CapabilityKey,
    pub supported: bool,
    pub source: String,
    pub confidence: String,
    pub profile_version: String,
    pub observed_at: String,
    pub valid_until: String,
}

#[derive(Default)]
pub(crate) struct CapabilityCache {
    loaded: bool,
    records: HashMap<CapabilityKey, CapabilityRecord>,
}

impl CapabilityCache {
    pub(crate) fn record(&mut self, record: CapabilityRecord) {
        let now = Utc::now();
        self.prune_expired(now);
        if record_is_fresh(&record, now) {
            self.insert(record);
        }
    }

    fn insert(&mut self, record: CapabilityRecord) {
        self.records.insert(record.key.clone(), record);
    }

    fn prune_expired(&mut self, now: DateTime<Utc>) {
        self.records
            .retain(|_, record| record_is_fresh(record, now));
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }
}

pub(crate) async fn restore(state: &AppState) -> AppResult<()> {
    let rows = load_records(state).await?;
    let mut cache = state.capability_profiles.lock().await;
    cache.records.clear();
    for record in rows {
        cache.insert(record);
    }
    cache.loaded = true;
    Ok(())
}

pub(crate) async fn capability_support(
    state: &AppState,
    provider_id: i64,
    model: &str,
    endpoint: &str,
    capability: &str,
) -> Option<bool> {
    ensure_loaded(state).await.ok()?;
    let mut cache = state.capability_profiles.lock().await;
    let now = Utc::now();
    cache.prune_expired(now);
    matching_candidates(&cache, provider_id, model, endpoint, capability, now)
        .into_iter()
        .max_by(|left, right| {
            source_rank(&left.source)
                .cmp(&source_rank(&right.source))
                .then_with(|| {
                    confidence_rank(&left.confidence).cmp(&confidence_rank(&right.confidence))
                })
                .then_with(|| left.profile_version.cmp(&right.profile_version))
                .then_with(|| left.observed_at.cmp(&right.observed_at))
        })
        .map(|record| record.supported)
}

pub(crate) async fn record_capability(
    state: &AppState,
    key: CapabilityKey,
    supported: bool,
    source: &str,
    confidence: &str,
) {
    let now = Utc::now();
    let valid_seconds = match source {
        SOURCE_RUNTIME => 90 * 24 * 60 * 60,
        _ => 365 * 24 * 60 * 60,
    };
    let observed_at = now.to_rfc3339();
    let valid_until = (now + Duration::seconds(valid_seconds)).to_rfc3339();
    let profile_version = match source {
        SOURCE_RUNTIME => "runtime-v1",
        SOURCE_TEMPLATE => "template-v1",
        _ => "manual-v1",
    };
    let record = CapabilityRecord {
        key: key.clone(),
        supported,
        source: source.to_string(),
        confidence: confidence.to_string(),
        profile_version: profile_version.to_string(),
        observed_at,
        valid_until,
    };

    let result = sqlx::query(
        "INSERT INTO provider_capability_profiles (
             provider_id, model, endpoint, capability, supported, source,
             confidence, profile_version, observed_at, valid_until
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(provider_id, model, endpoint, capability) DO UPDATE SET
             supported = excluded.supported,
             source = excluded.source,
             confidence = excluded.confidence,
             profile_version = excluded.profile_version,
             observed_at = excluded.observed_at,
             valid_until = excluded.valid_until
         WHERE
             CASE excluded.source
                 WHEN 'manual' THEN 3
                 WHEN 'template' THEN 2
                 ELSE 1
             END >=
             CASE provider_capability_profiles.source
                 WHEN 'manual' THEN 3
                 WHEN 'template' THEN 2
                 ELSE 1
             END",
    )
    .bind(key.provider_id)
    .bind(&key.model)
    .bind(&key.endpoint)
    .bind(&key.capability)
    .bind(supported as i64)
    .bind(source)
    .bind(confidence)
    .bind(profile_version)
    .bind(&record.observed_at)
    .bind(&record.valid_until)
    .execute(&state.pool)
    .await;

    match result {
        Ok(result) if result.rows_affected() > 0 => {
            state.capability_profiles.lock().await.record(record);
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(
                %error,
                provider_id = key.provider_id,
                model = %key.model,
                endpoint = %key.endpoint,
                capability = %key.capability,
                "failed to persist provider capability profile"
            );
        }
    }
}

/// Persists effort values learned from a precise upstream enum rejection.
///
/// This updates the synced model row so subsequent requests can normalize
/// before sending. Targets without a `provider_models` row still benefit from
/// the current in-flight retry.
pub(crate) async fn record_reasoning_efforts(
    state: &AppState,
    provider_id: i64,
    model: &str,
    efforts: &[String],
) {
    if efforts.is_empty() {
        return;
    }
    let Ok(encoded) = serde_json::to_string(efforts) else {
        return;
    };
    if let Err(error) = sqlx::query(
        "UPDATE provider_models SET reasoning_efforts = ? \
         WHERE provider_id = ? AND model_name = ?",
    )
    .bind(encoded)
    .bind(provider_id)
    .bind(model)
    .execute(&state.pool)
    .await
    {
        tracing::warn!(
            %error,
            provider_id,
            model,
            "failed to persist upstream reasoning effort compatibility"
        );
    }
}

async fn ensure_loaded(state: &AppState) -> AppResult<()> {
    if state.capability_profiles.lock().await.loaded {
        return Ok(());
    }
    let rows = load_records(state).await?;
    let mut cache = state.capability_profiles.lock().await;
    if !cache.loaded {
        cache.records.clear();
        for record in rows {
            cache.insert(record);
        }
        cache.loaded = true;
    }
    Ok(())
}

pub(crate) async fn prune_expired(state: &AppState) -> AppResult<u64> {
    let cutoff = Utc::now().to_rfc3339();
    let result = sqlx::query("DELETE FROM provider_capability_profiles WHERE valid_until <= ?")
        .bind(&cutoff)
        .execute(&state.pool)
        .await?;
    state
        .capability_profiles
        .lock()
        .await
        .prune_expired(Utc::now());
    Ok(result.rows_affected())
}

pub(crate) async fn remove_provider(state: &AppState, provider_id: i64) {
    state
        .capability_profiles
        .lock()
        .await
        .records
        .retain(|key, _| key.provider_id != provider_id);
}

async fn load_records(state: &AppState) -> AppResult<Vec<CapabilityRecord>> {
    let cutoff = Utc::now().to_rfc3339();
    sqlx::query("DELETE FROM provider_capability_profiles WHERE valid_until <= ?")
        .bind(cutoff)
        .execute(&state.pool)
        .await?;
    let rows = sqlx::query_as::<
        _,
        (
            i64,
            String,
            String,
            String,
            i64,
            String,
            String,
            String,
            String,
            String,
        ),
    >(
        "SELECT provider_id, model, endpoint, capability, supported, source, \
                confidence, profile_version, observed_at, valid_until \
         FROM provider_capability_profiles",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                provider_id,
                model,
                endpoint,
                capability,
                supported,
                source,
                confidence,
                profile_version,
                observed_at,
                valid_until,
            )| CapabilityRecord {
                key: CapabilityKey {
                    provider_id,
                    model,
                    endpoint,
                    capability,
                },
                supported: supported != 0,
                source,
                confidence,
                profile_version,
                observed_at,
                valid_until,
            },
        )
        .collect())
}

fn matching_candidates<'a>(
    cache: &'a CapabilityCache,
    provider_id: i64,
    model: &str,
    endpoint: &str,
    capability: &str,
    now: DateTime<Utc>,
) -> Vec<&'a CapabilityRecord> {
    [
        (model.to_string(), endpoint.to_string()),
        (model.to_string(), String::new()),
        (String::new(), endpoint.to_string()),
        (String::new(), String::new()),
    ]
    .into_iter()
    .filter_map(|(model, endpoint)| {
        cache.records.get(&CapabilityKey {
            provider_id,
            model,
            endpoint,
            capability: capability.to_string(),
        })
    })
    .filter(|record| record_is_fresh(record, now))
    .collect()
}

fn record_is_fresh(record: &CapabilityRecord, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(&record.valid_until)
        .map(|valid_until| valid_until.with_timezone(&Utc) > now)
        .unwrap_or(false)
}

fn source_rank(source: &str) -> u8 {
    match source {
        "manual" => 3,
        "template" => 2,
        "runtime" => 1,
        _ => 0,
    }
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "exact" => 3,
        "inferred" => 2,
        "unknown" => 1,
        _ => 0,
    }
}

#[cfg(test)]
#[path = "../tests/unit/capabilities.rs"]
mod tests;
