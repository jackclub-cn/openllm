use super::*;

/// Default request-body cap in MiB.
///
/// The gateway buffers the whole body in memory, so this is a memory-safety
/// guard rather than a protocol limit. Anthropic accepts up to 32 MiB, and a
/// handful of base64 images or a PDF easily exceeds the previous 16 MiB, which
/// made the gateway reject payloads the upstream would have served. Raise it
/// with `OPENLLM_MAX_BODY_MIB` when larger bodies are expected.
pub(crate) const DEFAULT_MAX_BODY_MIB: usize = 32;

/// Default cap on a buffered (non-streaming) upstream response in MiB.
///
/// LLM responses are small JSON documents, so this is a memory-safety guard
/// against a broken or hostile upstream streaming an unbounded body into
/// memory, not a protocol limit. Raise it with `OPENLLM_MAX_UPSTREAM_BODY_MIB`
/// on the rare endpoint that legitimately returns more.
pub(crate) const DEFAULT_MAX_UPSTREAM_BODY_MIB: usize = 64;

/// Default in-flight request-byte budget in MiB.
///
/// The gateway buffers every request body in memory, so a burst of large
/// coding-agent payloads can exhaust RAM long before the request-count cap
/// trips. This is an opt-in budget: `0` disables it, matching the request-count
/// cap. Set `OPENLLM_MAX_INFLIGHT_REQUEST_MIB` to a positive value to shed
/// requests whose combined buffered bodies would exceed it.
pub(crate) const DEFAULT_MAX_INFLIGHT_REQUEST_MIB: usize = 0;

/// Default admission wait in milliseconds.
///
/// `0` sheds an over-capacity request immediately, the non-blocking default.
/// A positive value lets the request wait that long for a slot before it is
/// shed, so a short coding-agent burst serializes instead of being dropped.
/// Override with `OPENLLM_ADMISSION_WAIT_MS`.
pub(crate) const DEFAULT_ADMISSION_WAIT_MS: u64 = 0;

/// Default hard cap on an upstream stream's total lifetime in seconds.
///
/// The idle timeout only bounds the gap *between* reads, so a stream that
/// trickles a keep-alive byte forever would hold its provider slot and memory
/// indefinitely. This cap bounds total stream duration, matching the largest
/// per-provider timeout budget with a margin. Zero disables it.
///
/// Override with `OPENLLM_STREAM_MAX_SECS`.
pub(crate) const DEFAULT_STREAM_MAX_SECS: u64 = 1260;

/// Default bound, in seconds, on how long the gateway will spend reading a
/// request body from a client.
///
/// Without it a slow or hostile client can hold an admission slot (and, with a
/// large body, memory) by trickling bytes forever. Zero disables the bound.
///
/// Override with `OPENLLM_BODY_READ_TIMEOUT_SECS`.
pub(crate) const DEFAULT_BODY_READ_TIMEOUT_SECS: u64 = 600;

/// Default fraction of the configured memory ceiling at which the gateway
/// starts shedding new proxied requests. A little headroom is left above it so
/// the process can still finish the work already in flight.
pub(crate) const DEFAULT_MEMORY_SHED_RATIO: f64 = 0.9;

/// Default wall-clock budget for a request's fallback loop, in seconds.
///
/// Once it elapses the gateway stops *starting* further route targets and
/// returns the last error, so a route full of failing targets cannot hold a
/// request (and its admission slot) for minutes. It never cuts an attempt that
/// is already running, so a slow-but-working provider is unaffected. Zero
/// disables the budget.
///
/// Override with `OPENLLM_REQUEST_TIMEOUT_SECS`.
pub(crate) const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 600;

/// Default number of consecutive provider failures before its circuit
/// hard-opens.
///
/// A provider below this threshold keeps the soft behaviour: its runtime
/// cooldown only deprioritises it (see `order_targets`) while an attempt is
/// still allowed, so a single transient blip on an otherwise healthy provider
/// does not turn into an outage. Once the streak reaches the threshold the
/// provider is short-circuited with a fast `503` and one request at a time is
/// admitted to probe recovery. Zero disables the hard-open and keeps the soft
/// cooldown only.
///
/// Override with `OPENLLM_PROVIDER_OPEN_THRESHOLD`.
pub(crate) const DEFAULT_PROVIDER_OPEN_THRESHOLD: u32 = 3;

/// Default first-token threshold above which a successful target is recorded
/// as slow. Only `latency_optimized` routes act on the sample, and the target
/// stays usable when it is the only candidate.
///
/// Override with `OPENLLM_SLOW_TARGET_MS`; `0` disables the penalty.
pub(crate) const DEFAULT_SLOW_TARGET_MS: i64 = 15_000;

/// How long one slow success keeps a target behind healthy siblings on a
/// latency-first route before it gets another probe. Repeated slow successes
/// escalate this duration.
pub(crate) const MAX_SLOW_TARGET_HOLD: Duration = Duration::from_secs(5 * 60);

/// A target with no slow sample for this long starts a fresh penalty streak.
pub(crate) const SLOW_TARGET_STREAK_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Minimum time a single half-open recovery probe may hold a provider's probe
/// slot before another probe is admitted. Providers with a longer configured
/// request timeout extend this lease so a slow probe is never pre-empted.
///
/// The lease only bounds the case where a probe is claimed but never settles
/// (for example the request fell over to a healthier target before it reached
/// this provider, so no success or failure was recorded). It is comfortably
/// longer than a normal upstream header wait so a slow probe is not pre-empted.
pub(crate) const PROVIDER_PROBE_LEASE: Duration = Duration::from_secs(120);

/// Default idle gap allowed between bytes from an upstream.
///
/// A total-request timeout would abort long generations (reasoning models
/// routinely run past five minutes), so only the gap between reads is bounded.
/// Raise it with `OPENLLM_UPSTREAM_IDLE_TIMEOUT_SECS` for upstreams that stay
/// silent for longer before the first byte.
pub(crate) const DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS: u64 = 300;

/// Default gap after which a silent downstream SSE stream gets a keep-alive
/// comment so load balancers, proxies, and clients do not drop the connection
/// while the model is still thinking. Zero disables the comments.
///
/// Override with `OPENLLM_SSE_KEEPALIVE_SECS`.
pub(crate) const DEFAULT_SSE_KEEPALIVE_SECS: u64 = 15;

/// Default grace period to let in-flight requests finish after a shutdown
/// signal before the remaining connections are dropped. Zero waits forever.
///
/// Override with `OPENLLM_SHUTDOWN_GRACE_SECS`.
pub(crate) const DEFAULT_SHUTDOWN_GRACE_SECS: u64 = 30;

/// Parses an operator-provided request-body cap, falling back to the default
/// for empty, unparsable, or non-positive values.
pub(crate) fn parse_max_body_mib(value: Option<&str>) -> usize {
    parse_body_mib(value, DEFAULT_MAX_BODY_MIB)
}

/// Parses a MiB cap, falling back to `default` for empty, unparsable, or
/// non-positive values so a typo can never remove the guard.
pub(crate) fn parse_body_mib(value: Option<&str>, default: usize) -> usize {
    parse_positive(value)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
}

/// Parses an operator-provided idle timeout in seconds, falling back to
/// `default` for empty, unparsable, or zero values.
pub(crate) fn parse_positive_secs(value: Option<&str>, default: u64) -> u64 {
    parse_positive(value).unwrap_or(default)
}

/// Parses an operator-provided SSE keep-alive interval.
///
/// Unlike the idle timeout, `0` is meaningful here: it disables the comments.
/// Anything missing or unparsable keeps the default so a typo cannot silently
/// turn the protection off.
pub(crate) fn parse_keepalive_secs(value: Option<&str>) -> Option<Duration> {
    parse_optional_secs(value, DEFAULT_SSE_KEEPALIVE_SECS)
}

/// Parses an optional "N seconds" bound where an explicit `0` disables it.
///
/// A missing or unparsable value keeps `default` so a typo cannot silently
/// remove a safety bound; an explicit `0` turns the bound off on purpose.
pub(crate) fn parse_optional_secs(value: Option<&str>, default: u64) -> Option<Duration> {
    match value.map(str::trim) {
        None | Some("") => Some(Duration::from_secs(default)),
        Some("0") => None,
        Some(raw) => Some(Duration::from_secs(raw.parse::<u64>().unwrap_or(default))),
    }
}

/// Parses an operator-provided global concurrency cap.
///
/// Empty, zero, and unparsable values all disable the cap, matching the
/// opt-in default: no admission limit unless an operator asks for one.
pub(crate) fn parse_concurrency_limit(value: Option<&str>) -> Option<usize> {
    value
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|limit| *limit > 0)
}

/// Parses an operator-provided in-flight request-byte budget in MiB.
///
/// Empty, zero, and unparsable values all disable the budget, matching the
/// opt-in default so an upgrade never starts shedding bodies by surprise.
pub(crate) fn parse_inflight_bytes_mib(value: Option<&str>) -> Option<usize> {
    parse_concurrency_limit(value).map(|mib| mib.saturating_mul(1024 * 1024))
}

/// Parses the admission wait budget, in milliseconds.
///
/// `0` (and any unparsable value) keeps the non-blocking default: an
/// over-capacity request is shed at once. A positive value lets it wait that
/// long for a slot before the `429`, so a short burst serializes instead of
/// being dropped while the gateway is only momentarily saturated.
pub(crate) fn parse_admission_wait_ms(value: Option<&str>) -> Duration {
    let default = Duration::from_millis(DEFAULT_ADMISSION_WAIT_MS);
    match value.map(str::trim) {
        None | Some("") => default,
        Some(raw) => raw
            .parse::<u64>()
            .map(Duration::from_millis)
            .unwrap_or(default),
    }
}

/// Resolves the memory ceiling and its source for the pressure guard.
///
/// `configured` is the raw `OPENLLM_MEMORY_LIMIT_MIB` value and `auto` is the
/// ceiling discovered from the container. An explicit positive value wins, an
/// explicit `0` disables the guard, and an unset or blank value falls back to
/// the discovered limit, so container deployments are protected without any
/// tuning while bare-metal hosts (no cgroup limit) stay unaffected.
pub(crate) fn resolve_memory_limit(
    configured: Option<&str>,
    auto: Option<u64>,
) -> (Option<u64>, &'static str) {
    match configured.map(str::trim) {
        Some("0") => (None, "off"),
        Some(value) if !value.is_empty() => {
            match value.parse::<u64>().ok().filter(|mib| *mib > 0) {
                Some(mib) => (Some(mib.saturating_mul(1024 * 1024)), "env"),
                None => resolve_auto_memory_limit(auto),
            }
        }
        _ => resolve_auto_memory_limit(auto),
    }
}

pub(super) fn resolve_auto_memory_limit(auto: Option<u64>) -> (Option<u64>, &'static str) {
    match auto {
        Some(bytes) => (Some(bytes), "cgroup"),
        None => (None, "off"),
    }
}

/// Parses a cgroup memory limit file's contents.
///
/// cgroup v2 writes `max` when no limit is set; cgroup v1 writes an enormous
/// sentinel. Both, plus anything at or above 1 PiB, are treated as "no limit"
/// so an unconstrained host never arms the guard.
// Used by the Linux cgroup reader; other platforms exercise it through tests.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_cgroup_memory_max(raw: Option<&str>) -> Option<u64> {
    let raw = raw?.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("max") {
        return None;
    }
    let bytes = raw.parse::<u64>().ok()?;
    (bytes > 0 && bytes < (1u64 << 50)).then_some(bytes)
}

/// Parses the memory shed ratio, falling back to `default` unless the value is
/// a finite fraction in `(0, 1]`.
pub(crate) fn parse_shed_ratio(value: Option<&str>, default: f64) -> f64 {
    match value.map(str::trim) {
        None | Some("") => default,
        Some(raw) => raw
            .parse::<f64>()
            .ok()
            .filter(|ratio| ratio.is_finite() && *ratio > 0.0 && *ratio <= 1.0)
            .unwrap_or(default),
    }
}

/// Parses the post-signal drain grace period.
///
/// A missing or unparsable value keeps the default; an explicit `0` waits
/// indefinitely, preserving the pre-deadline behavior for operators who want
/// every stream to finish.
pub(crate) fn parse_shutdown_grace_secs(value: Option<&str>) -> u64 {
    match value.map(str::trim) {
        None | Some("") => DEFAULT_SHUTDOWN_GRACE_SECS,
        Some(raw) => raw.parse::<u64>().unwrap_or(DEFAULT_SHUTDOWN_GRACE_SECS),
    }
}

pub(super) fn parse_positive(value: Option<&str>) -> Option<u64> {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
}

/// Resolves the configured request-body cap in bytes.
pub(crate) fn max_request_body_bytes() -> usize {
    parse_max_body_mib(std::env::var("OPENLLM_MAX_BODY_MIB").ok().as_deref())
        .saturating_mul(1024 * 1024)
}

/// Resolves the configured buffered-upstream-response cap in bytes.
pub(crate) fn max_upstream_body_bytes() -> usize {
    parse_body_mib(
        std::env::var("OPENLLM_MAX_UPSTREAM_BODY_MIB")
            .ok()
            .as_deref(),
        DEFAULT_MAX_UPSTREAM_BODY_MIB,
    )
    .saturating_mul(1024 * 1024)
}

/// Resolves the configured hard cap on an upstream stream's total lifetime.
pub(crate) fn max_stream_lifetime() -> Option<Duration> {
    parse_optional_secs(
        std::env::var("OPENLLM_STREAM_MAX_SECS").ok().as_deref(),
        DEFAULT_STREAM_MAX_SECS,
    )
}

/// Resolves the configured bound on reading a request body from a client.
pub(crate) fn body_read_timeout() -> Option<Duration> {
    parse_optional_secs(
        std::env::var("OPENLLM_BODY_READ_TIMEOUT_SECS")
            .ok()
            .as_deref(),
        DEFAULT_BODY_READ_TIMEOUT_SECS,
    )
}

/// Resolves the configured total wall-clock budget for a request's fallback
/// loop.
pub(crate) fn request_timeout() -> Option<Duration> {
    parse_optional_secs(
        std::env::var("OPENLLM_REQUEST_TIMEOUT_SECS")
            .ok()
            .as_deref(),
        DEFAULT_REQUEST_TIMEOUT_SECS,
    )
}

/// Resolves the consecutive-failure threshold at which a provider's circuit
/// hard-opens.
pub(crate) fn provider_open_threshold() -> u32 {
    parse_provider_open_threshold(
        std::env::var("OPENLLM_PROVIDER_OPEN_THRESHOLD")
            .ok()
            .as_deref(),
    )
}

/// Parses the provider circuit-open threshold.
///
/// An explicit `0` disables the hard-open (the soft cooldown still applies); a
/// missing or unparsable value keeps the default so a typo cannot silently
/// remove the protection.
pub(crate) fn parse_provider_open_threshold(value: Option<&str>) -> u32 {
    match value.map(str::trim) {
        None | Some("") => DEFAULT_PROVIDER_OPEN_THRESHOLD,
        Some(raw) => raw
            .parse::<u32>()
            .unwrap_or(DEFAULT_PROVIDER_OPEN_THRESHOLD),
    }
}

/// Resolves the first-token threshold for soft target deprioritization.
pub(crate) fn slow_target_threshold_ms() -> i64 {
    static THRESHOLD_MS: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *THRESHOLD_MS.get_or_init(|| {
        parse_slow_target_threshold_ms(std::env::var("OPENLLM_SLOW_TARGET_MS").ok().as_deref())
    })
}

/// Parses the first-token threshold. An explicit `0` disables the penalty; a
/// missing or unparsable value keeps the default so a typo cannot disable it.
pub(crate) fn parse_slow_target_threshold_ms(value: Option<&str>) -> i64 {
    match value.map(str::trim) {
        None | Some("") => DEFAULT_SLOW_TARGET_MS,
        Some(raw) => raw.parse::<i64>().unwrap_or(DEFAULT_SLOW_TARGET_MS).max(0),
    }
}
