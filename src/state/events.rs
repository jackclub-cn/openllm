use super::*;

/// In-memory state for a successful target whose first token was slow. Read by
/// `latency_optimized` routing only.
#[derive(Debug, Clone, Copy, Default)]
pub struct TargetSlowState {
    pub(crate) until: Option<Instant>,
    pub(crate) slow_streak: u32,
    pub(crate) last_first_token_ms: i64,
    pub(crate) last_slow_at: Option<Instant>,
}

/// Derives a provider's circuit-breaker state from its failure streak and
/// whether a cooldown is currently active.
///
/// Matches `admit_provider_attempt`: below the threshold the circuit is closed
/// (the cooldown only deprioritises the provider), at or above it the provider
/// is `open` while cooling and `half_open` once the cooldown has elapsed and a
/// recovery probe is allowed.
pub(crate) fn provider_circuit_state(streak: u32, cooling: bool, threshold: u32) -> &'static str {
    if threshold == 0 || streak < threshold {
        "closed"
    } else if cooling {
        "open"
    } else {
        "half_open"
    }
}

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub id: i64,
    pub request_id: String,
    pub success: bool,
    pub streamed: bool,
}

/// How many runtime cooldown entries a bulk clear removed, per map.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClearedCooldowns {
    pub(crate) providers: usize,
    pub(crate) models: usize,
    pub(crate) provider_keys: usize,
    pub(crate) sinks: usize,
}
