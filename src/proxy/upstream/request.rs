use super::*;

pub(crate) fn join_upstream_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = format!("/{}", path.trim_start_matches('/'));
    if base.ends_with("/v1") && path.starts_with("/v1/") {
        format!("{base}{}", &path[3..])
    } else {
        format!("{base}{path}")
    }
}

pub(crate) fn apply_custom_headers(
    mut request: RequestBuilder,
    headers: &str,
) -> AppResult<RequestBuilder> {
    let Ok(headers) = serde_json::from_str::<Value>(headers) else {
        return Ok(request);
    };
    let Some(headers) = headers.as_object() else {
        return Ok(request);
    };
    for (name, value) in headers {
        let Some(value) = value.as_str() else {
            continue;
        };
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            AppError::BadRequest(format!("invalid header name '{name}': {error}"))
        })?;
        let value = HeaderValue::from_str(value).map_err(|error| {
            AppError::BadRequest(format!("invalid value for header '{name}': {error}"))
        })?;
        request = request.header(name, value);
    }
    Ok(request)
}
