use super::*;

pub(in crate::api) fn apply_usage_filters<'a>(
    builder: &mut QueryBuilder<'a, Sqlite>,
    filters: &'a UsageFilters,
) {
    if let Some(provider_id) = filters.provider_id {
        builder.push(" AND u.provider_id = ").push_bind(provider_id);
    }
    if let Some(provider_api_key_id) = filters.provider_api_key_id {
        builder
            .push(" AND u.provider_api_key_id = ")
            .push_bind(provider_api_key_id);
    }
    if let Some(api_key_id) = filters.api_key_id {
        builder.push(" AND u.api_key_id = ").push_bind(api_key_id);
    }
    if let Some(route_id) = filters.route_id {
        builder.push(" AND u.route_id = ").push_bind(route_id);
    }
    if let Some(model) = &filters.model
        && !model.trim().is_empty()
    {
        builder
            .push(" AND u.requested_model LIKE ")
            .push_bind(format!("%{}%", model.trim()));
    }
    if let Some(request_id) = &filters.request_id
        && !request_id.trim().is_empty()
    {
        builder
            .push(" AND u.request_id LIKE ")
            .push_bind(format!("%{}%", request_id.trim()));
    }
    if let Some(session_id) = &filters.session_id
        && !session_id.trim().is_empty()
    {
        builder
            .push(" AND u.session_id LIKE ")
            .push_bind(format!("%{}%", session_id.trim()));
    }
    if let Some(endpoint) = &filters.endpoint
        && !endpoint.trim().is_empty()
    {
        builder
            .push(" AND u.endpoint LIKE ")
            .push_bind(format!("%{}%", endpoint.trim()));
    }
    if let Some(success) = filters.success {
        builder.push(" AND u.success = ").push_bind(success as i64);
        if !success {
            builder.push(" AND u.in_flight = 0");
        }
    }
    if let Some(in_flight) = filters.in_flight {
        builder
            .push(" AND u.in_flight = ")
            .push_bind(in_flight as i64);
    }
    if let Some(gateway_adjusted) = filters.gateway_adjusted {
        if gateway_adjusted {
            builder.push(" AND u.warning_message IS NOT NULL AND TRIM(u.warning_message) <> ''");
        } else {
            builder.push(" AND (u.warning_message IS NULL OR TRIM(u.warning_message) = '')");
        }
    }
}

pub(in crate::api) fn apply_usage_time_range<'a>(
    builder: &mut QueryBuilder<'a, Sqlite>,
    from: Option<&'a str>,
    to: Option<&'a str>,
) {
    if let Some(from) = from {
        builder.push(" AND u.created_at >= ").push_bind(from);
    }
    if let Some(to) = to {
        builder.push(" AND u.created_at <= ").push_bind(to);
    }
}

pub(in crate::api) fn usage_query_is_unfiltered(query: &UsageQuery) -> bool {
    query.provider_id.is_none()
        && query.provider_api_key_id.is_none()
        && query.api_key_id.is_none()
        && query.route_id.is_none()
        && query
            .model
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && query
            .request_id
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && query
            .session_id
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && query
            .endpoint
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && query.success.is_none()
        && query.in_flight.is_none()
        && query.gateway_adjusted.is_none()
        && query.from.is_none()
        && query.to.is_none()
}
