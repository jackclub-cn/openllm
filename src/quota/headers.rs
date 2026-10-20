use super::*;

pub(super) fn records_from_rate_limit_headers(
    provider_id: i64,
    provider_key_id: Option<i64>,
    model: &str,
    headers: &HeaderMap,
) -> Vec<ProviderQuotaSnapshotRecord> {
    let now = Utc::now();
    let mut records = Vec::new();
    for (window_key, unit, limit_names, remaining_names, reset_names) in [
        (
            "requests",
            "request",
            &[
                "x-ratelimit-limit-requests",
                "anthropic-ratelimit-requests-limit",
            ][..],
            &[
                "x-ratelimit-remaining-requests",
                "anthropic-ratelimit-requests-remaining",
            ][..],
            &[
                "x-ratelimit-reset-requests",
                "anthropic-ratelimit-requests-reset",
            ][..],
        ),
        (
            "tokens",
            "token",
            &[
                "x-ratelimit-limit-tokens",
                "anthropic-ratelimit-tokens-limit",
            ][..],
            &[
                "x-ratelimit-remaining-tokens",
                "anthropic-ratelimit-tokens-remaining",
            ][..],
            &[
                "x-ratelimit-reset-tokens",
                "anthropic-ratelimit-tokens-reset",
            ][..],
        ),
        (
            "custom",
            "request",
            &["x-ratelimit-limit"][..],
            &["x-ratelimit-remaining"][..],
            &["x-ratelimit-reset"][..],
        ),
    ] {
        let limit = first_header_f64(headers, limit_names);
        let remaining = first_header_f64(headers, remaining_names);
        if limit.is_none() && remaining.is_none() {
            continue;
        }
        let reset_at = first_header_value(headers, reset_names)
            .and_then(|value| parse_header_reset(value, now));
        records.push(ProviderQuotaSnapshotRecord {
            provider_id,
            provider_key_id,
            model: model.to_string(),
            window_key: window_key.to_string(),
            used: limit
                .zip(remaining)
                .map(|(limit, remaining)| (limit - remaining).max(0.0)),
            limit,
            remaining,
            unit: unit.to_string(),
            reset_at,
            source: SOURCE_RESPONSE_HEADER.to_string(),
            confidence: if limit.is_some() && remaining.is_some() {
                "exact"
            } else {
                "inferred"
            }
            .to_string(),
            observed_at: now.to_rfc3339(),
            valid_until: (now + ChronoDuration::seconds(ttl_seconds_for(window_key))).to_rfc3339(),
        });
    }
    records
}

pub(super) fn first_header_value<'a>(headers: &'a HeaderMap, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(super) fn first_header_f64(headers: &HeaderMap, names: &[&str]) -> Option<f64> {
    first_header_value(headers, names).and_then(|value| {
        value
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
    })
}

pub(super) fn parse_header_reset(value: &str, now: DateTime<Utc>) -> Option<String> {
    if let Some(timestamp) = parse_rfc3339(value) {
        return Some(timestamp.to_rfc3339());
    }
    if let Ok(number) = value.parse::<i64>() {
        if number >= 1_000_000_000_000 {
            if let Some(timestamp) = DateTime::<Utc>::from_timestamp_millis(number) {
                return Some(timestamp.to_rfc3339());
            }
        } else if number >= 1_000_000_000 {
            if let Some(timestamp) = DateTime::<Utc>::from_timestamp(number, 0) {
                return Some(timestamp.to_rfc3339());
            }
        } else {
            return Some((now + ChronoDuration::seconds(number)).to_rfc3339());
        }
    }
    parse_compact_duration(value).map(|duration| {
        (now + ChronoDuration::seconds(duration.as_secs().min(i64::MAX as u64) as i64)).to_rfc3339()
    })
}

pub(super) fn parse_compact_duration(value: &str) -> Option<StdDuration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<f64>() {
        return (seconds.is_finite() && seconds >= 0.0)
            .then(|| StdDuration::from_secs_f64(seconds));
    }
    let bytes = value.as_bytes();
    let mut index = 0usize;
    let mut total = 0.0f64;
    let mut parsed = false;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let number_start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            index += 1;
        }
        if number_start == index {
            return None;
        }
        let number = value[number_start..index].parse::<f64>().ok()?;
        if !number.is_finite() || number < 0.0 {
            return None;
        }
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let unit_start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        let unit = &value[unit_start..index].to_ascii_lowercase();
        let multiplier = match unit.as_str() {
            "ms" => 0.001,
            "" | "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600.0,
            "d" | "day" | "days" => 86_400.0,
            _ => return None,
        };
        total += number * multiplier;
        parsed = true;
    }
    parsed.then(|| StdDuration::from_secs_f64(total))
}
