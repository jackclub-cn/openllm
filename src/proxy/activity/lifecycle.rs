use super::*;

struct ActivityGuard {
    state: AppState,
    request_id: String,
    handed_off: bool,
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        if !self.handed_off {
            self.state
                .activity_cancel(&self.request_id, "client_cancelled");
        }
    }
}

pub(crate) async fn track_activity_request<F>(
    state: &AppState,
    request_id: &str,
    endpoint: &str,
    future: F,
) -> AppResult<Response>
where
    F: std::future::Future<Output = AppResult<Response>>,
{
    state.activity_start(request_id, endpoint);
    let mut guard = ActivityGuard {
        state: state.clone(),
        request_id: request_id.to_string(),
        handed_off: false,
    };
    let result = future.await;
    finish_activity_for_pre_response(state, request_id, &result);
    guard.handed_off = true;
    result.map(|response| {
        let (parts, body) = response.into_parts();
        let mut stream = body.into_data_stream();
        let guard = ActivityGuard {
            state: state.clone(),
            request_id: request_id.to_string(),
            handed_off: false,
        };
        let observed = async_stream::stream! {
            // Capture the guard even when the response body is never polled.
            let mut guard = guard;
            while let Some(chunk) = stream.next().await { yield chunk; }
            guard.handed_off = true;
        };
        Response::from_parts(parts, Body::from_stream(observed))
    })
}
