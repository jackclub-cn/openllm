use super::*;

pub(in crate::proxy) fn provider_health_rank(health: Option<i64>) -> u8 {
    match health {
        Some(0) => 2,
        Some(_) => 0,
        None => 1,
    }
}

pub(in crate::proxy) const PROVIDER_KEY_TOUCH_INTERVAL: Duration = Duration::from_secs(60);

pub(in crate::proxy) fn bounded_retry_after(duration: Duration, max: Duration) -> Option<Duration> {
    (!duration.is_zero()).then(|| duration.min(max))
}

/// Exponential backoff for a same-target retry, capped by the operator setting.
pub(in crate::proxy) fn retry_backoff(
    settings: &crate::models::ResilienceSettings,
    retry_index: u32,
) -> Duration {
    retry_backoff_with_draw(settings, retry_index, rand::random::<f64>())
}

/// Deterministic form of [`retry_backoff`] used by tests.
///
/// The jitter only extends the delay, never shortens it, so an operator's
/// configured minimum remains a real floor. The configured cap still wins.
pub(in crate::proxy) fn retry_backoff_with_draw(
    settings: &crate::models::ResilienceSettings,
    retry_index: u32,
    draw: f64,
) -> Duration {
    let base = settings.retry_backoff_ms.max(0) as u64;
    let cap = settings.retry_max_backoff_ms.max(0) as u64;
    let multiplier = 1u64 << retry_index.min(6);
    let delay = base.saturating_mul(multiplier).min(cap);
    let jitter_percent = (draw.clamp(0.0, 1.0) * 25.0).round() as u64;
    let jittered = delay.saturating_mul(100u64.saturating_add(jitter_percent)) / 100;
    Duration::from_millis(jittered.min(cap))
}

pub(in crate::proxy) fn retry_after_from_headers(headers: &HeaderMap) -> Option<Duration> {
    if let Some(value) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        return bounded_retry_after(Duration::from_millis(value), MAX_UPSTREAM_RETRY_AFTER);
    }

    if let Some(value) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
    {
        if let Ok(seconds) = value.parse::<u64>() {
            return bounded_retry_after(Duration::from_secs(seconds), MAX_UPSTREAM_RETRY_AFTER);
        }

        if let Ok(deadline) = httpdate::parse_http_date(value)
            && let Ok(duration) = deadline.duration_since(std::time::SystemTime::now())
        {
            return bounded_retry_after(duration, MAX_UPSTREAM_RETRY_AFTER);
        }
    }

    retry_after_from_reset_headers(headers)
}

/// Parses the rate-limit reset headers used by common OpenAI and Anthropic
/// compatible upstreams. When several windows are present, wait for the latest
/// one so the next attempt does not immediately hit another active limit.
pub(super) fn retry_after_from_reset_headers(headers: &HeaderMap) -> Option<Duration> {
    const RESET_AFTER_HEADERS: [&str; 2] = ["x-ratelimit-reset-after", "ratelimit-reset-after"];
    const RESET_DURATION_HEADERS: [&str; 2] =
        ["x-ratelimit-reset-requests", "x-ratelimit-reset-tokens"];
    const RESET_TIMESTAMP_HEADERS: [&str; 3] = [
        "anthropic-ratelimit-unified-reset",
        "x-ratelimit-reset",
        "ratelimit-reset",
    ];

    let mut latest = None;
    for name in RESET_AFTER_HEADERS {
        if let Some(duration) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_reset_after)
        {
            latest = Some(latest.map_or(duration, |current: Duration| current.max(duration)));
        }
    }
    for name in RESET_DURATION_HEADERS {
        if let Some(duration) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_reset_duration)
        {
            latest = Some(latest.map_or(duration, |current: Duration| current.max(duration)));
        }
    }
    for name in RESET_TIMESTAMP_HEADERS {
        if let Some(duration) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_reset_timestamp)
        {
            latest = Some(latest.map_or(duration, |current: Duration| current.max(duration)));
        }
    }
    latest.and_then(|duration| bounded_retry_after(duration, MAX_UPSTREAM_RETRY_AFTER))
}

pub(super) fn parse_reset_after(value: &str) -> Option<Duration> {
    let seconds = value.trim().parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds).ok()
}

pub(super) fn parse_reset_duration(value: &str) -> Option<Duration> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    if let Some(seconds) = parse_reset_after(&value) {
        return Some(seconds);
    }

    let mut total = Duration::ZERO;
    let mut rest = value.as_str();
    let mut parsed_any = false;
    while !rest.is_empty() {
        let number_end = rest
            .find(|character: char| !character.is_ascii_digit() && character != '.')
            .unwrap_or(rest.len());
        if number_end == 0 {
            return None;
        }
        let number = rest[..number_end].parse::<f64>().ok()?;
        rest = &rest[number_end..];

        let unit_end = rest
            .find(|character: char| character.is_ascii_digit() || character == '.')
            .unwrap_or(rest.len());
        if unit_end == 0 {
            return None;
        }
        let multiplier = match &rest[..unit_end] {
            "ns" => 1e-9,
            "us" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return None,
        };
        let seconds = number * multiplier;
        if !seconds.is_finite() || seconds < 0.0 {
            return None;
        }
        total = total.saturating_add(Duration::try_from_secs_f64(seconds).ok()?);
        rest = &rest[unit_end..];
        parsed_any = true;
    }
    parsed_any.then_some(total)
}

pub(super) fn parse_reset_timestamp(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(raw) = value.parse::<i64>() {
        // Unix reset timestamps may be seconds or milliseconds.
        let seconds = if raw > 10_000_000_000 {
            raw / 1000
        } else {
            raw
        };
        let now = Utc::now().timestamp();
        return (seconds > now).then(|| Duration::from_secs((seconds - now) as u64));
    }
    let deadline = chrono::DateTime::parse_from_rfc3339(value).ok()?;
    let duration = deadline
        .with_timezone(&Utc)
        .signed_duration_since(Utc::now())
        .to_std()
        .ok()?;
    Some(duration)
}

pub(in crate::proxy) fn provider_cooldown(
    status: Option<StatusCode>,
    failures: u32,
    retry_after: Option<Duration>,
    configured_seconds: Option<i64>,
) -> Duration {
    let default_base = match status {
        Some(StatusCode::TOO_MANY_REQUESTS) => Duration::from_secs(30),
        _ => Duration::from_secs(20),
    };
    let configured_base = configured_seconds
        .filter(|seconds| *seconds > 0)
        .map(|seconds| Duration::from_secs(seconds.unsigned_abs()));
    let base = match configured_base {
        Some(configured) => configured.max(retry_after.unwrap_or_default()),
        None => retry_after.unwrap_or(default_base),
    };
    let exponent = failures.saturating_sub(1).min(4);
    let cap = if configured_base.is_some() {
        MAX_CONFIGURED_PROVIDER_COOLDOWN
    } else {
        MAX_PROVIDER_COOLDOWN
    };
    base.saturating_mul(1u32 << exponent).min(cap)
}

pub(in crate::proxy) fn provider_key_cooldown(
    status: StatusCode,
    retry_after: Option<Duration>,
) -> Duration {
    let base = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Duration::from_secs(300),
        StatusCode::TOO_MANY_REQUESTS => Duration::from_secs(30),
        _ => Duration::from_secs(20),
    };
    retry_after
        .map(|duration| base.max(duration))
        .unwrap_or(base)
        .min(MAX_PROVIDER_KEY_COOLDOWN)
}
