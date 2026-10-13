use super::*;

/// Live view of every in-memory cooldown that currently removes a target from
/// routing.
///
/// Cooldowns are process-local by design: they exist to shed load from a
/// provider that is failing right now. That makes them invisible to operators
/// unless the gateway exposes them, so an incident responder cannot tell whether
/// a model is out because of an outage or because of a cooldown that should
/// already have expired.
#[derive(Debug, serde::Serialize)]
pub struct CooldownSnapshot {
    pub provider_cooldowns: Vec<ProviderCooldownView>,
    pub model_cooldowns: Vec<ModelCooldownView>,
    pub provider_key_cooldowns: Vec<ProviderKeyCooldownView>,
    /// Consecutive failures that hard-open a provider's circuit; `0` keeps the
    /// soft cooldown only.
    pub provider_open_threshold: u32,
    pub generated_at: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ProviderCooldownView {
    pub provider_id: i64,
    pub provider_name: Option<String>,
    /// Whole seconds left before the provider re-enters routing (minimum 1).
    /// `0` means the cooldown has elapsed and the provider is half-open.
    pub remaining_seconds: i64,
    /// Consecutive failures that produced the current escalated cooldown.
    pub failure_streak: u32,
    /// Circuit breaker state: `open`, `half_open`, or `closed`.
    pub state: &'static str,
}

#[derive(Debug, serde::Serialize)]
pub struct ModelCooldownView {
    pub provider_id: i64,
    pub provider_name: Option<String>,
    pub model: String,
    pub remaining_seconds: i64,
}

#[derive(Debug, serde::Serialize)]
pub struct ProviderKeyCooldownView {
    pub provider_key_id: i64,
    pub provider_name: Option<String>,
    pub key_name: Option<String>,
    pub remaining_seconds: i64,
}

pub async fn list_cooldowns(State(state): State<AppState>) -> AppResult<Json<CooldownSnapshot>> {
    Ok(Json(cooldown_snapshot(&state).await?))
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct ClearCooldownsInput {
    /// Clears every provider, model and credential cooldown at once.
    #[serde(default)]
    pub all: bool,
    /// Restricts a provider-scoped clear. With `model` unset this clears the
    /// provider cooldown and every model cooldown under it.
    #[serde(default)]
    pub provider_id: Option<i64>,
    /// Restricts the clear to one upstream model on `provider_id`.
    #[serde(default)]
    pub model: Option<String>,
    /// Clears one upstream credential's cooldown.
    #[serde(default)]
    pub provider_key_id: Option<i64>,
}

#[derive(Debug, serde::Serialize)]
pub struct ClearCooldownsResult {
    pub cleared_provider_cooldowns: usize,
    pub cleared_model_cooldowns: usize,
    pub cleared_provider_key_cooldowns: usize,
}

pub async fn clear_cooldowns(
    State(state): State<AppState>,
    Json(input): Json<ClearCooldownsInput>,
) -> AppResult<Json<ClearCooldownsResult>> {
    let model = input
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if !input.all {
        if let Some(provider_key_id) = input.provider_key_id {
            let removed = state
                .provider_key_cooldown
                .lock()
                .await
                .remove(&provider_key_id)
                .is_some();
            if removed {
                state.mark_provider_key_resilience_dirty(provider_key_id);
            }
            let cleared = ClearCooldownsResult {
                cleared_provider_cooldowns: 0,
                cleared_model_cooldowns: 0,
                cleared_provider_key_cooldowns: usize::from(removed),
            };
            record_audit(
                &state,
                "clear",
                "cooldowns",
                Some(&provider_key_id.to_string()),
                "cleared provider key cooldown",
                Some(json!({ "provider_key_id": provider_key_id })),
            )
            .await;
            return Ok(Json(cleared));
        }
        let Some(provider_id) = input.provider_id else {
            return Err(AppError::BadRequest(
                "specify all, provider_key_id, or provider_id".to_string(),
            ));
        };
        let cleared = clear_provider_scope(&state, provider_id, model).await;
        record_audit(
            &state,
            "clear",
            "cooldowns",
            Some(&provider_id.to_string()),
            match model {
                Some(_) => "cleared model cooldown",
                None => "cleared provider cooldowns",
            },
            Some(json!({ "provider_id": provider_id, "model": model })),
        )
        .await;
        return Ok(Json(cleared));
    }

    // One call captures the keys to purge, empties the maps and only then marks
    // them dirty: marking first would let the writer persist the cooldowns this
    // request is clearing, and marking after the clear without the captured keys
    // would mark nothing at all and leave the rows in the mirror.
    let cleared_all = state.clear_all_resilience_state().await;
    let cleared = ClearCooldownsResult {
        cleared_provider_cooldowns: cleared_all.providers,
        cleared_model_cooldowns: cleared_all.models,
        cleared_provider_key_cooldowns: cleared_all.provider_keys,
    };
    record_audit(
        &state,
        "clear",
        "cooldowns",
        None,
        "cleared all routing cooldowns",
        Some(json!({
            "providers": cleared.cleared_provider_cooldowns,
            "models": cleared.cleared_model_cooldowns,
            "provider_keys": cleared.cleared_provider_key_cooldowns,
        })),
    )
    .await;
    Ok(Json(cleared))
}

async fn clear_provider_scope(
    state: &AppState,
    provider_id: i64,
    model: Option<&str>,
) -> ClearCooldownsResult {
    let cleared_models = {
        let mut cooldowns = state.target_cooldown.lock().await;
        let mut removed: Vec<(i64, String)> = Vec::new();
        match model {
            Some(model) => {
                let key = (provider_id, model.to_string());
                if cooldowns.remove(&key).is_some() {
                    removed.push(key);
                }
            }
            None => cooldowns.retain(|key @ (id, _), _| {
                if *id == provider_id {
                    removed.push(key.clone());
                    false
                } else {
                    true
                }
            }),
        }
        removed
    };
    let cleared_model_cooldowns = cleared_models.len();
    for (provider_id, model) in cleared_models {
        state.mark_target_resilience_dirty(provider_id, &model);
    }

    // Clearing one model leaves the provider circuit alone: sibling models may
    // still be failing. A provider-wide clear also resets the escalation streak
    // so the next failure starts from the base cooldown again.
    let cleared_provider_cooldowns = if model.is_some() {
        0
    } else {
        state.provider_failure_streak.lock().await.remove(&provider_id);
        state.provider_probe.lock().await.remove(&provider_id);
        state.mark_provider_resilience_dirty(provider_id);
        usize::from(
            state
                .provider_cooldown
                .lock()
                .await
                .remove(&provider_id)
                .is_some(),
        )
    };

    ClearCooldownsResult {
        cleared_provider_cooldowns,
        cleared_model_cooldowns,
        cleared_provider_key_cooldowns: 0,
    }
}

/// Drops expired entries and resolves display names for the surviving ones.
pub(crate) async fn cooldown_snapshot(state: &AppState) -> AppResult<CooldownSnapshot> {
    let now = std::time::Instant::now();

    let threshold = crate::state::provider_open_threshold();
    let failure_streaks = state.provider_failure_streak.lock().await.clone();
    let provider_cooldowns = {
        let mut cooldowns = state.provider_cooldown.lock().await;
        cooldowns.retain(|_, until| *until > now);
        let mut entries = cooldowns
            .iter()
            .map(|(provider_id, until)| {
                let streak = failure_streaks.get(provider_id).copied().unwrap_or(0);
                (
                    *provider_id,
                    remaining_seconds(*until, now),
                    crate::state::provider_circuit_state(streak, true, threshold),
                )
            })
            .collect::<Vec<_>>();
        // A half-open provider has no active cooldown left but is still
        // short-circuited, so surface it too, or an operator cannot see why it
        // keeps failing fast.
        for (provider_id, streak) in &failure_streaks {
            if !cooldowns.contains_key(provider_id)
                && crate::state::provider_circuit_state(*streak, false, threshold) == "half_open"
            {
                entries.push((*provider_id, 0, "half_open"));
            }
        }
        entries.sort_by_key(|(provider_id, _, _)| *provider_id);
        entries
    };
    let model_cooldowns = {
        let mut cooldowns = state.target_cooldown.lock().await;
        cooldowns.retain(|_, until| *until > now);
        let mut entries = cooldowns
            .iter()
            .map(|((provider_id, model), until)| {
                (*provider_id, model.clone(), remaining_seconds(*until, now))
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries
    };
    let provider_key_cooldowns = {
        let mut cooldowns = state.provider_key_cooldown.lock().await;
        cooldowns.retain(|_, until| *until > now);
        let mut entries = cooldowns
            .iter()
            .map(|(key_id, until)| (*key_id, remaining_seconds(*until, now)))
            .collect::<Vec<_>>();
        entries.sort_by_key(|(key_id, _)| *key_id);
        entries
    };
    let provider_names = provider_name_map(state).await?;
    let key_names = provider_key_name_map(state).await?;

    Ok(CooldownSnapshot {
        provider_cooldowns: provider_cooldowns
            .into_iter()
            .map(
                |(provider_id, remaining_seconds, state)| ProviderCooldownView {
                    provider_name: provider_names.get(&provider_id).cloned(),
                    failure_streak: failure_streaks.get(&provider_id).copied().unwrap_or(0),
                    provider_id,
                    remaining_seconds,
                    state,
                },
            )
            .collect(),
        model_cooldowns: model_cooldowns
            .into_iter()
            .map(
                |(provider_id, model, remaining_seconds)| ModelCooldownView {
                    provider_name: provider_names.get(&provider_id).cloned(),
                    provider_id,
                    model,
                    remaining_seconds,
                },
            )
            .collect(),
        provider_key_cooldowns: provider_key_cooldowns
            .into_iter()
            .map(|(provider_key_id, remaining_seconds)| {
                let (provider_name, key_name) = key_names
                    .get(&provider_key_id)
                    .cloned()
                    .unwrap_or((None, None));
                ProviderKeyCooldownView {
                    provider_key_id,
                    provider_name,
                    key_name,
                    remaining_seconds,
                }
            })
            .collect(),
        provider_open_threshold: threshold,
        generated_at: Utc::now().to_rfc3339(),
    })
}

fn remaining_seconds(until: std::time::Instant, now: std::time::Instant) -> i64 {
    let remaining = until.saturating_duration_since(now);
    (remaining.as_secs_f64().ceil() as i64).max(1)
}

async fn provider_name_map(state: &AppState) -> AppResult<HashMap<i64, String>> {
    let rows = sqlx::query_as::<_, (i64, String)>("SELECT id, name FROM providers")
        .fetch_all(&state.pool)
        .await?;
    Ok(rows.into_iter().collect())
}

async fn provider_key_name_map(
    state: &AppState,
) -> AppResult<HashMap<i64, (Option<String>, Option<String>)>> {
    let rows = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT k.id, p.name, k.name
         FROM provider_api_keys k
         JOIN providers p ON p.id = k.provider_id",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(key_id, provider_name, key_name)| {
            (key_id, (Some(provider_name), Some(key_name)))
        })
        .collect())
}
