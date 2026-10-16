//! Persistence for runtime resilience state.
//!
//! Provider circuits, per-model cooldowns and per-credential cooldowns live in
//! memory because the request hot path must not take a SQLite write lock to
//! record a failure. That is correct while the process runs, but a restart
//! (deploy, crash, host reboot) used to clear every cooldown at once: the first
//! request after the restart went straight back to the provider that had just
//! been failing, which is exactly the stampede the circuit breaker exists to
//! prevent.
//!
//! This module mirrors that state into SQLite with absolute expiry deadlines
//! and rebuilds it on startup:
//!
//! * a mutation marks the affected entity dirty and wakes one writer task;
//! * the writer coalesces a burst of failures into a single transaction, so the
//!   hot path never waits on a write;
//! * rows expire on their own deadline, and a half-open provider (a failure
//!   streak with no running cooldown) is only restored while it is recent;
//! * the persisted rows are never used to gate a request that disagrees with
//!   the in-memory state — memory stays authoritative.

use std::collections::HashSet;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::error::AppResult;
use crate::state::{AppState, TargetSinkKey};

/// Default for `OPENLLM_RESILIENCE_STATE_PERSIST`.
///
/// Persistence is on by default: the whole point is that a restart does not
/// hand traffic back to a provider that is still unhealthy. Operators who run
/// several gateways against the same database (unsupported) can turn it off.
pub(crate) const DEFAULT_RESILIENCE_STATE_PERSIST: bool = true;

/// Longest cooldown deadline that may be restored, in seconds.
///
/// The largest cooldown the gateway can hand out is one hour, so anything past
/// that is a sign of clock skew or hand-edited data; clamping keeps a broken
/// clock from gating a provider indefinitely.
pub(crate) const MAX_RESTORED_COOLDOWN_SECS: i64 = 60 * 60;

/// Longest target sink deadline that may be restored.
///
/// Sinks are intentionally short-lived ordering hints. A row past this bound is
/// treated as stale rather than letting a bad clock suppress a target.
pub(crate) const MAX_RESTORED_SINK_SECS: i64 = 5 * 60;

/// How long a provider's half-open marker stays restorable, in seconds.
///
/// After a long shutdown the provider is assumed to have recovered, so a
/// restart does not gate it with a probe it never earned. An *active* cooldown
/// is unaffected: its own deadline decides.
pub(crate) const HALF_OPEN_RESTORE_TTL_SECS: i64 = 60 * 60;

/// Coalescing window for a burst of failures and cooldowns.
const WRITE_DEBOUNCE: Duration = Duration::from_millis(250);

/// How often expired rows are swept when nothing changes.
const SWEEP_INTERVAL: Duration = Duration::from_secs(300);

/// Parses `OPENLLM_RESILIENCE_STATE_PERSIST`.
///
/// A missing or unparsable value keeps the default (on) so a typo cannot
/// silently make restarts forget a tripped circuit; an explicit `0`/`false`/
/// `no`/`off` disables the mirror.
pub(crate) fn parse_resilience_state_persist(value: Option<&str>) -> bool {
    match value.map(str::trim) {
        None | Some("") => DEFAULT_RESILIENCE_STATE_PERSIST,
        Some(raw) => !matches!(
            raw.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
    }
}

/// Configured switch for the resilience-state mirror.
pub(crate) fn resilience_state_persist() -> bool {
    parse_resilience_state_persist(
        std::env::var("OPENLLM_RESILIENCE_STATE_PERSIST")
            .ok()
            .as_deref(),
    )
}

fn now_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// Converts a monotonic deadline into an absolute one.
fn deadline_unix_millis(now: Instant, until: Instant) -> i64 {
    let remaining = until.saturating_duration_since(now);
    now_unix_millis().saturating_add(remaining.as_millis() as i64)
}

/// Remaining lifetime of an absolute deadline, `None` once it has elapsed.
fn remaining_from_millis(deadline_ms: i64, now_unix_ms: i64) -> Option<Duration> {
    (deadline_ms > now_unix_ms).then(|| Duration::from_millis((deadline_ms - now_unix_ms) as u64))
}

/// Entities whose persisted mirror no longer matches memory.
///
/// The writer drains this set instead of re-serializing every provider, so a
/// busy gateway writes only what actually changed. It also lets a cleared
/// cooldown schedule the *deletion* of its row.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DirtyResilienceState {
    pub providers: HashSet<i64>,
    pub targets: HashSet<(i64, String)>,
    pub provider_keys: HashSet<i64>,
    pub sinks: HashSet<TargetSinkKey>,
}

impl DirtyResilienceState {
    pub(crate) fn is_empty(&self) -> bool {
        self.providers.is_empty()
            && self.targets.is_empty()
            && self.provider_keys.is_empty()
            && self.sinks.is_empty()
    }
}

/// Rebuilds the in-memory resilience state from SQLite.
///
/// Called once at startup, before the server accepts traffic. Returns how many
/// entities were restored so the caller can log it. Expired rows are deleted
/// here, which is what makes the mirror self-limiting: nothing has to run while
/// the process is down for a cooldown to expire.
pub(crate) async fn restore(state: &AppState) -> AppResult<usize> {
    if !state.resilience_state_persist {
        return Ok(0);
    }
    let now = Instant::now();
    let now_unix_ms = now_unix_millis();
    let half_open_floor_ms = now_unix_ms.saturating_sub(HALF_OPEN_RESTORE_TTL_SECS * 1000);
    let max_deadline_ms = now_unix_ms.saturating_add(MAX_RESTORED_COOLDOWN_SECS * 1000);
    let max_sink_deadline_ms = now_unix_ms.saturating_add(MAX_RESTORED_SINK_SECS * 1000);

    sqlx::query(
        "DELETE FROM provider_circuit_state WHERE failure_streak <= 0 OR updated_at_unix_ms < ?",
    )
    .bind(half_open_floor_ms)
    .execute(&state.pool)
    .await?;
    sqlx::query("DELETE FROM target_cooldown_state WHERE cooldown_until_unix_ms <= ?")
        .bind(now_unix_ms)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM provider_key_cooldown_state WHERE cooldown_until_unix_ms <= ?")
        .bind(now_unix_ms)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM target_sink_state WHERE sink_until_unix_ms <= ?")
        .bind(now_unix_ms)
        .execute(&state.pool)
        .await?;

    let providers = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT provider_id, failure_streak, cooldown_until_unix_ms
         FROM provider_circuit_state
         WHERE failure_streak > 0 AND updated_at_unix_ms >= ?",
    )
    .bind(half_open_floor_ms)
    .fetch_all(&state.pool)
    .await?;
    let targets = sqlx::query_as::<_, (i64, String, i64)>(
        "SELECT provider_id, model, cooldown_until_unix_ms
         FROM target_cooldown_state
         WHERE cooldown_until_unix_ms > ?",
    )
    .bind(now_unix_ms)
    .fetch_all(&state.pool)
    .await?;
    let provider_keys = sqlx::query_as::<_, (i64, i64)>(
        "SELECT provider_key_id, cooldown_until_unix_ms
         FROM provider_key_cooldown_state
         WHERE cooldown_until_unix_ms > ?",
    )
    .bind(now_unix_ms)
    .fetch_all(&state.pool)
    .await?;
    let sinks = sqlx::query_as::<_, (i64, i64, String, i64, String)>(
        "SELECT provider_id, provider_key_id, model, sink_until_unix_ms, reason_code
         FROM target_sink_state
         WHERE sink_until_unix_ms > ?",
    )
    .bind(now_unix_ms)
    .fetch_all(&state.pool)
    .await?;

    let restored_providers = providers.len();
    let restored_targets = targets.len();
    let restored_keys = provider_keys.len();
    let restored_sinks = sinks.len();
    if restored_providers + restored_targets + restored_keys + restored_sinks == 0 {
        return Ok(0);
    }

    {
        let mut streaks = state.provider_failure_streak.lock().await;
        let mut cooldowns = state.provider_cooldown.lock().await;
        for (provider_id, streak, deadline_ms) in providers {
            let deadline_ms = deadline_ms.clamp(0, max_deadline_ms);
            streaks.insert(
                provider_id,
                u32::try_from(streak.max(0)).unwrap_or(u32::MAX),
            );
            // An elapsed deadline is inserted as an already-expired instant:
            // that is the half-open state, so exactly one request probes
            // recovery instead of every request hitting the provider at once.
            cooldowns.insert(
                provider_id,
                now + remaining_from_millis(deadline_ms, now_unix_ms).unwrap_or_default(),
            );
        }
    }
    {
        let mut cooldowns = state.target_cooldown.lock().await;
        for (provider_id, model, deadline_ms) in targets {
            let deadline_ms = deadline_ms.clamp(0, max_deadline_ms);
            if let Some(remaining) = remaining_from_millis(deadline_ms, now_unix_ms) {
                cooldowns.insert((provider_id, model), now + remaining);
            }
        }
    }
    {
        let mut cooldowns = state.provider_key_cooldown.lock().await;
        for (provider_key_id, deadline_ms) in provider_keys {
            let deadline_ms = deadline_ms.clamp(0, max_deadline_ms);
            if let Some(remaining) = remaining_from_millis(deadline_ms, now_unix_ms) {
                cooldowns.insert(provider_key_id, now + remaining);
            }
        }
    }
    {
        let mut target_sink = state.target_sink.lock().await;
        for (provider_id, provider_key_id, model, deadline_ms, reason_code) in sinks {
            let deadline_ms = deadline_ms.clamp(0, max_sink_deadline_ms);
            if let Some(remaining) = remaining_from_millis(deadline_ms, now_unix_ms) {
                target_sink.insert(
                    (
                        provider_id,
                        (provider_key_id > 0).then_some(provider_key_id),
                        model,
                    ),
                    crate::state::TargetSinkEntry {
                        until: now + remaining,
                        reason_code: restore_sink_reason(&reason_code),
                        observed_at: now,
                    },
                );
            }
        }
    }

    tracing::info!(
        providers = restored_providers,
        models = restored_targets,
        provider_keys = restored_keys,
        sinks = restored_sinks,
        "restored persisted resilience state; cooldowns survive the restart"
    );
    Ok(restored_providers + restored_targets + restored_keys + restored_sinks)
}

fn restore_sink_reason(reason_code: &str) -> &'static str {
    match reason_code {
        "local_capacity" => "local_capacity",
        "rate_limited" => "rate_limited",
        "quota_exhausted" => "quota_exhausted",
        _ => "restored_sink",
    }
}

/// Writes every dirty entity's current state, in one transaction.
///
/// Memory stays authoritative: each entity is re-read here, so a cooldown that
/// was cleared between the mutation and the flush deletes its row instead of
/// resurrecting it.
pub(crate) async fn flush(state: &AppState) -> AppResult<()> {
    if !state.resilience_state_persist {
        return Ok(());
    }
    let (providers, targets, provider_keys, sinks) = {
        let mut dirty = state
            .resilience_dirty
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if dirty.is_empty() {
            return Ok(());
        }
        let taken = std::mem::take(&mut *dirty);
        (
            taken.providers.into_iter().collect::<Vec<_>>(),
            taken.targets.into_iter().collect::<Vec<_>>(),
            taken.provider_keys.into_iter().collect::<Vec<_>>(),
            taken.sinks.into_iter().collect::<Vec<_>>(),
        )
    };

    match write_dirty(state, &providers, &targets, &provider_keys, &sinks).await {
        Ok(()) => Ok(()),
        Err(error) => {
            // Keep the marks: a transient SQLite failure must not lose the
            // cooldown that the next restart is supposed to remember.
            let mut dirty = state
                .resilience_dirty
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            dirty.providers.extend(providers);
            dirty.targets.extend(targets);
            dirty.provider_keys.extend(provider_keys);
            dirty.sinks.extend(sinks);
            Err(error)
        }
    }
}

async fn write_dirty(
    state: &AppState,
    providers: &[i64],
    targets: &[(i64, String)],
    provider_keys: &[i64],
    sinks: &[TargetSinkKey],
) -> AppResult<()> {
    let now = Instant::now();
    let now_unix_ms = now_unix_millis();

    // Rows are keyed by foreign keys, so an entity that was deleted while its
    // state was dirty is dropped instead of aborting the whole transaction.
    let live_providers: HashSet<i64> = sqlx::query_scalar("SELECT id FROM providers")
        .fetch_all(&state.pool)
        .await?
        .into_iter()
        .collect();
    let live_provider_keys: HashSet<i64> = sqlx::query_scalar("SELECT id FROM provider_api_keys")
        .fetch_all(&state.pool)
        .await?
        .into_iter()
        .collect();

    let streaks = state.provider_failure_streak.lock().await.clone();
    let cooldowns = state.provider_cooldown.lock().await.clone();
    let target_cooldowns = state.target_cooldown.lock().await.clone();
    let key_cooldowns = state.provider_key_cooldown.lock().await.clone();
    let target_sinks = state.target_sink.lock().await.clone();

    let mut transaction = state.pool.begin().await?;
    for provider_id in providers {
        let streak = streaks.get(provider_id).copied().unwrap_or(0);
        if !live_providers.contains(provider_id) || streak == 0 {
            sqlx::query("DELETE FROM provider_circuit_state WHERE provider_id = ?")
                .bind(provider_id)
                .execute(&mut *transaction)
                .await?;
            continue;
        }
        let deadline_ms = cooldowns
            .get(provider_id)
            .map(|until| deadline_unix_millis(now, *until))
            .unwrap_or(0);
        sqlx::query(
            "INSERT INTO provider_circuit_state (
                 provider_id, failure_streak, cooldown_until_unix_ms, updated_at_unix_ms
             ) VALUES (?, ?, ?, ?)
             ON CONFLICT(provider_id) DO UPDATE SET
                 failure_streak = excluded.failure_streak,
                 cooldown_until_unix_ms = excluded.cooldown_until_unix_ms,
                 updated_at_unix_ms = excluded.updated_at_unix_ms",
        )
        .bind(provider_id)
        .bind(i64::from(streak))
        .bind(deadline_ms)
        .bind(now_unix_ms)
        .execute(&mut *transaction)
        .await?;
    }

    for (provider_id, model) in targets {
        let deadline_ms = target_cooldowns
            .get(&(*provider_id, model.clone()))
            .map(|until| deadline_unix_millis(now, *until))
            .filter(|deadline_ms| *deadline_ms > now_unix_ms);
        match deadline_ms {
            Some(deadline_ms) if live_providers.contains(provider_id) => {
                sqlx::query(
                    "INSERT INTO target_cooldown_state (
                         provider_id, model, cooldown_until_unix_ms
                     ) VALUES (?, ?, ?)
                     ON CONFLICT(provider_id, model) DO UPDATE SET
                         cooldown_until_unix_ms = excluded.cooldown_until_unix_ms",
                )
                .bind(provider_id)
                .bind(model)
                .bind(deadline_ms)
                .execute(&mut *transaction)
                .await?;
            }
            _ => {
                sqlx::query(
                    "DELETE FROM target_cooldown_state WHERE provider_id = ? AND model = ?",
                )
                .bind(provider_id)
                .bind(model)
                .execute(&mut *transaction)
                .await?;
            }
        }
    }

    for provider_key_id in provider_keys {
        let deadline_ms = key_cooldowns
            .get(provider_key_id)
            .map(|until| deadline_unix_millis(now, *until))
            .filter(|deadline_ms| *deadline_ms > now_unix_ms);
        match deadline_ms {
            Some(deadline_ms) if live_provider_keys.contains(provider_key_id) => {
                sqlx::query(
                    "INSERT INTO provider_key_cooldown_state (
                         provider_key_id, cooldown_until_unix_ms
                     ) VALUES (?, ?)
                     ON CONFLICT(provider_key_id) DO UPDATE SET
                         cooldown_until_unix_ms = excluded.cooldown_until_unix_ms",
                )
                .bind(provider_key_id)
                .bind(deadline_ms)
                .execute(&mut *transaction)
                .await?;
            }
            _ => {
                sqlx::query("DELETE FROM provider_key_cooldown_state WHERE provider_key_id = ?")
                    .bind(provider_key_id)
                    .execute(&mut *transaction)
                    .await?;
            }
        }
    }

    for key in sinks {
        let (provider_id, provider_key_id, model) = key;
        let deadline_ms = target_sinks
            .get(key)
            .map(|entry| deadline_unix_millis(now, entry.until))
            .filter(|deadline_ms| *deadline_ms > now_unix_ms);
        let key_id = provider_key_id.unwrap_or(0);
        let key_live = key_id == 0 || live_provider_keys.contains(&key_id);
        match deadline_ms {
            Some(deadline_ms) if live_providers.contains(provider_id) && key_live => {
                let reason_code = target_sinks
                    .get(key)
                    .map(|entry| entry.reason_code)
                    .unwrap_or("restored_sink");
                sqlx::query(
                    "INSERT INTO target_sink_state (
                         provider_id, provider_key_id, model, sink_until_unix_ms, reason_code
                     ) VALUES (?, ?, ?, ?, ?)
                     ON CONFLICT(provider_id, provider_key_id, model) DO UPDATE SET
                         sink_until_unix_ms = excluded.sink_until_unix_ms,
                         reason_code = excluded.reason_code",
                )
                .bind(provider_id)
                .bind(key_id)
                .bind(model)
                .bind(deadline_ms)
                .bind(reason_code)
                .execute(&mut *transaction)
                .await?;
            }
            _ => {
                sqlx::query(
                    "DELETE FROM target_sink_state
                     WHERE provider_id = ? AND provider_key_id = ? AND model = ?",
                )
                .bind(provider_id)
                .bind(key_id)
                .bind(model)
                .execute(&mut *transaction)
                .await?;
            }
        }
    }
    transaction.commit().await?;

    // Drop in-memory state for entities that no longer exist, so a deleted
    // provider cannot keep short-circuiting a request path that still
    // references its id.
    for provider_id in providers {
        if !live_providers.contains(provider_id) {
            state
                .provider_failure_streak
                .lock()
                .await
                .remove(provider_id);
            state.provider_cooldown.lock().await.remove(provider_id);
            state.provider_probe.lock().await.remove(provider_id);
            state
                .target_cooldown
                .lock()
                .await
                .retain(|(id, _), _| id != provider_id);
        }
    }
    Ok(())
}

/// Deletes rows whose deadline has passed.
///
/// Provider rows survive an elapsed cooldown on purpose (that is the half-open
/// marker), so only rows whose streak is gone or whose marker is stale are
/// removed here.
async fn sweep_expired(state: &AppState) -> AppResult<()> {
    let now_unix_ms = now_unix_millis();
    let half_open_floor_ms = now_unix_ms.saturating_sub(HALF_OPEN_RESTORE_TTL_SECS * 1000);
    sqlx::query(
        "DELETE FROM provider_circuit_state WHERE failure_streak <= 0 OR updated_at_unix_ms < ?",
    )
    .bind(half_open_floor_ms)
    .execute(&state.pool)
    .await?;
    sqlx::query("DELETE FROM target_cooldown_state WHERE cooldown_until_unix_ms <= ?")
        .bind(now_unix_ms)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM provider_key_cooldown_state WHERE cooldown_until_unix_ms <= ?")
        .bind(now_unix_ms)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM target_sink_state WHERE sink_until_unix_ms <= ?")
        .bind(now_unix_ms)
        .execute(&state.pool)
        .await?;
    Ok(())
}

/// Long-lived writer that mirrors resilience state into SQLite.
///
/// A single task keeps writes off the request path and coalesces a burst of
/// failures into one transaction. Failures to write are logged and retried on
/// the next state change (or sweep); they never fail a proxied request.
pub(crate) async fn run_writer(state: AppState) {
    loop {
        let dirty = tokio::select! {
            _ = state.resilience_flush.notified() => {
                // Let a burst of failures land in one transaction.
                tokio::time::sleep(WRITE_DEBOUNCE).await;
                true
            }
            _ = tokio::time::sleep(SWEEP_INTERVAL) => false,
        };
        let result = if dirty {
            flush(&state).await
        } else {
            sweep_expired(&state).await
        };
        if let Err(error) = result {
            tracing::warn!(%error, "failed to persist resilience state");
        }
    }
}
