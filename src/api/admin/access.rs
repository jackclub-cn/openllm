use super::*;

/// Enforces the management credential, when one is configured.
pub async fn admin_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> AppResult<Response> {
    if let Some(expected) = &state.admin_token {
        let token = request
            .headers()
            .get("x-admin-token")
            .and_then(|value| value.to_str().ok())
            .or_else(|| bearer_token(request.headers()));

        if token != Some(expected.as_str()) {
            return Err(AppError::Unauthorized(
                "a valid admin token is required".to_string(),
            ));
        }
    }

    Ok(next.run(request).await)
}

/// Rejects browser requests from origins outside the same-origin/allowlist set.
///
/// CORS controls whether a browser may read a response; this guard additionally
/// stops a cross-origin page from submitting a side-effecting "simple" request
/// that browsers send without a preflight.
pub async fn admin_origin_guard(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> AppResult<Response> {
    let Some(origin) = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(next.run(request).await);
    };

    let same_origin = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .zip(origin_authority(origin))
        .is_some_and(|(host, authority)| host.eq_ignore_ascii_case(authority));
    let allowed = same_origin
        || state
            .cors_origins
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(origin));
    if !allowed {
        return Err(AppError::Forbidden(
            "cross-origin management access is not allowed".to_string(),
        ));
    }
    Ok(next.run(request).await)
}

fn origin_authority(origin: &str) -> Option<&str> {
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .map(|value| value.trim_end_matches('/'))
        .filter(|value| !value.is_empty() && !value.contains('/'))
}
