use super::*;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const EVENT_TICKET_TTL: Duration = Duration::from_secs(30);
const MAX_EVENT_TICKETS: usize = 1024;

#[derive(Debug, Serialize)]
pub struct EventStreamTicket {
    pub ticket: String,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Deserialize)]
pub struct EventStreamQuery {
    pub ticket: Option<String>,
}

/// Issues a short-lived, one-use credential for an `EventSource` connection.
///
/// Browsers cannot attach headers to `EventSource`, so the persistent admin
/// token must never be placed in the URL. This ticket only authorizes one
/// stream open and expires quickly, limiting what a proxy access log can leak.
pub async fn event_stream_ticket(State(state): State<AppState>) -> AppResult<Response> {
    let ticket = uuid::Uuid::new_v4().simple().to_string();
    let now = Instant::now();
    let mut tickets = state.event_stream_tickets.lock().await;
    tickets.retain(|_, expires_at| *expires_at > now);
    if tickets.len() >= MAX_EVENT_TICKETS
        && let Some(oldest) = tickets
            .iter()
            .min_by_key(|(_, expires_at)| **expires_at)
            .map(|(ticket, _)| ticket.clone())
    {
        tickets.remove(&oldest);
    }
    tickets.insert(ticket.clone(), now + EVENT_TICKET_TTL);
    drop(tickets);

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(EventStreamTicket {
            ticket,
            expires_in_seconds: EVENT_TICKET_TTL.as_secs(),
        }),
    )
        .into_response())
}

pub async fn event_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<EventStreamQuery>,
) -> Response {
    let header_authorized = state.admin_token.as_ref().is_some_and(|expected| {
        headers
            .get("x-admin-token")
            .and_then(|value| value.to_str().ok())
            .or_else(|| bearer_token(&headers))
            == Some(expected.as_str())
    });
    let ticket_authorized = match query.ticket.as_deref() {
        Some(ticket) => take_event_ticket(&state, ticket).await,
        None => false,
    };
    if !header_authorized && !ticket_authorized {
        return AppError::Unauthorized("a valid event ticket is required".to_string())
            .into_response();
    }

    let mut usage_receiver = state.events.subscribe();
    let mut activity_receiver = state.activity_events.subscribe();
    let stream = async_stream::stream! {
        yield Ok::<Bytes, std::convert::Infallible>(Bytes::from_static(b"event: ready\ndata: {}\n\n"));
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(20));
        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    yield Ok(Bytes::from_static(b": keep-alive\n\n"));
                }
                event = usage_receiver.recv() => {
                    match event {
                        Ok(event) => {
                            let payload = json!({
                                "id": event.id,
                                "request_id": event.request_id,
                                "success": event.success,
                                "streamed": event.streamed
                            });
                            yield Ok(Bytes::from(format!("event: usage\ndata: {payload}\n\n")));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                event = activity_receiver.recv() => {
                    match event {
                        Ok(event) => {
                            let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
                            yield Ok(Bytes::from(format!("event: activity\ndata: {payload}\n\n")));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            yield Ok(Bytes::from_static(
                                b"event: activity_resync\ndata: {}\n\n",
                            ));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn take_event_ticket(state: &AppState, ticket: &str) -> bool {
    let mut tickets = state.event_stream_tickets.lock().await;
    let now = Instant::now();
    tickets.retain(|_, expires_at| *expires_at > now);
    tickets.remove(ticket).is_some()
}
