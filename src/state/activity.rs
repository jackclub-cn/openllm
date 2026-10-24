use super::*;
use crate::models::{ContextBreakdown, Usage};
use chrono::Utc;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

const DEFAULT_FINISHED_CAPACITY: usize = 256;
const DEFAULT_FINISHED_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_ACTIVITY_CANDIDATES: usize = 32;
const MAX_ACTIVITY_ATTEMPTS: usize = 32;
const OFF_VALUES: &[&str] = &["0", "false", "off", "disabled", "none"];

mod attempts;
mod bridge;
mod observation;
mod timing;
mod transitions;
mod types;
pub use types::*;

struct ActivityRecord {
    snapshot: RequestActivitySnapshot,
    started_instant: Instant,
    finished_instant: Option<Instant>,
    route_set_instant: Option<Instant>,
    first_token_instant: Option<Instant>,
    attempt_started: Option<Instant>,
    upstream_started: Option<Instant>,
    phase_started: Instant,
    input_reported: bool,
    cache_reported: bool,
    initial_context: Option<ContextBreakdown>,
}

struct ActivityInner {
    records: HashMap<String, ActivityRecord>,
    finished_order: VecDeque<String>,
    /// One terminal snapshot survives normal TTL and capacity cleanup.
    last_finished: Option<RequestActivitySnapshot>,
    sequence: u64,
    instance_id: String,
    active_count: usize,
    events: Option<broadcast::Sender<ActivityEvent>>,
}

pub struct RequestActivityStore {
    enabled: bool,
    finished_capacity: usize,
    finished_ttl: Duration,
    inner: Arc<Mutex<ActivityInner>>,
}

impl RequestActivityStore {
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn from_env(events: broadcast::Sender<ActivityEvent>) -> Self {
        let enabled = std::env::var("OPENLLM_LIVE_REQUEST_TRACE")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .is_none_or(|value| !OFF_VALUES.contains(&value.as_str()));
        Self {
            enabled,
            finished_capacity: DEFAULT_FINISHED_CAPACITY,
            finished_ttl: DEFAULT_FINISHED_TTL,
            inner: Arc::new(Mutex::new(ActivityInner {
                records: HashMap::new(),
                finished_order: VecDeque::new(),
                last_finished: None,
                sequence: 0,
                instance_id: uuid::Uuid::new_v4().to_string(),
                active_count: 0,
                events: Some(events),
            })),
        }
    }

    #[cfg(test)]
    fn for_test(finished_capacity: usize, finished_ttl: Duration) -> Self {
        Self {
            enabled: true,
            finished_capacity,
            finished_ttl,
            inner: Arc::new(Mutex::new(ActivityInner {
                records: HashMap::new(),
                finished_order: VecDeque::new(),
                last_finished: None,
                sequence: 0,
                instance_id: uuid::Uuid::new_v4().to_string(),
                active_count: 0,
                events: None,
            })),
        }
    }

    pub fn start(&self, request_id: &str, endpoint: &str) -> Option<ActivityEvent> {
        self.start_at(request_id, endpoint, Instant::now())
    }

    fn start_at(
        &self,
        request_id: &str,
        endpoint: &str,
        started: Instant,
    ) -> Option<ActivityEvent> {
        if !self.enabled {
            return None;
        }
        let now = Utc::now();
        let snapshot = RequestActivitySnapshot {
            request_id: request_id.to_string(),
            revision: 0,
            started_at: now.to_rfc3339(),
            updated_at: now.to_rfc3339(),
            finished_at: None,
            phase: "admitted".to_string(),
            outcome: "running".to_string(),
            endpoint: endpoint.to_string(),
            streamed: false,
            session_id: None,
            api_key_id: None,
            api_key_name: None,
            route_id: None,
            route_name: None,
            strategy: None,
            requested_model: None,
            target: None,
            request_tokens_estimated: 0,
            context: None,
            prompt_tokens: None,
            completion_tokens: None,
            status_code: None,
            error_code: None,
            waiting_reason: None,
            fallback_count: 0,
            attempt_count: 0,
            candidate_count: 0,
            candidates_truncated: false,
            attempts_truncated: false,
            timings: ActivityTimings::default(),
            cache: ActivityCache {
                state: "unknown".to_string(),
                ..ActivityCache::default()
            },
            candidates: Vec::new(),
            attempts: Vec::new(),
        };
        let mut inner = self.lock();
        inner.cleanup(self.finished_capacity, self.finished_ttl);
        if inner.records.contains_key(request_id) {
            return None;
        }
        inner.records.insert(
            request_id.to_string(),
            ActivityRecord {
                snapshot,
                started_instant: started,
                finished_instant: None,
                route_set_instant: None,
                first_token_instant: None,
                attempt_started: None,
                upstream_started: None,
                phase_started: started,
                input_reported: false,
                cache_reported: false,
                initial_context: None,
            },
        );
        inner.active_count += 1;
        Some(inner.event("started", request_id))
    }

    pub fn snapshot(&self, request_id: &str) -> Option<RequestActivitySnapshot> {
        let mut inner = self.lock();
        inner.cleanup(self.finished_capacity, self.finished_ttl);
        if let Some(mut snapshot) = inner
            .records
            .get(request_id)
            .map(|record| record.snapshot.clone())
        {
            inner.refresh_timings(request_id, &mut snapshot);
            return Some(snapshot);
        }
        inner
            .last_finished
            .as_ref()
            .filter(|snapshot| snapshot.request_id == request_id)
            .cloned()
    }

    pub fn recent(&self, limit: usize) -> ActivityList {
        let mut inner = self.lock();
        inner.cleanup(self.finished_capacity, self.finished_ttl);
        let active_count = inner.active_count;
        let mut items = inner
            .records
            .iter()
            .map(|(request_id, record)| {
                let mut snapshot = record.snapshot.clone();
                inner.refresh_timings(request_id, &mut snapshot);
                snapshot
            })
            .collect::<Vec<_>>();
        if let Some(last_finished) = &inner.last_finished
            && !inner.records.contains_key(&last_finished.request_id)
        {
            items.push(last_finished.clone());
        }
        items.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then_with(|| right.revision.cmp(&left.revision))
        });
        items.truncate(limit.min(200));
        ActivityList {
            enabled: self.enabled,
            generated_at: Utc::now().to_rfc3339(),
            active_count,
            sequence: inner.sequence,
            instance_id: inner.instance_id.clone(),
            items,
        }
    }

    fn update<F>(&self, request_id: &str, kind: &'static str, update: F) -> Option<ActivityEvent>
    where
        F: FnOnce(&mut ActivityRecord) -> bool,
    {
        self.update_record(request_id, kind, false, update)
    }

    fn update_record<F>(
        &self,
        request_id: &str,
        kind: &'static str,
        allow_terminal: bool,
        update: F,
    ) -> Option<ActivityEvent>
    where
        F: FnOnce(&mut ActivityRecord) -> bool,
    {
        if !self.enabled {
            return None;
        }
        let mut inner = self.lock();
        inner.cleanup(self.finished_capacity, self.finished_ttl);
        let (became_finished, finished_snapshot) = {
            let record = inner.records.get_mut(request_id)?;
            let was_finished = record.finished_instant.is_some();
            if (!allow_terminal && record.finished_instant.is_some()) || !update(record) {
                return None;
            }
            record.snapshot.revision = record.snapshot.revision.saturating_add(1);
            record.snapshot.updated_at = Utc::now().to_rfc3339();
            let became_finished = !was_finished && record.finished_instant.is_some();
            (
                became_finished,
                record
                    .finished_instant
                    .is_some()
                    .then(|| record.snapshot.clone()),
            )
        };
        if let Some(snapshot) = finished_snapshot {
            inner.remember_last_finished(snapshot);
        }
        if became_finished {
            inner.finished_order.push_back(request_id.to_string());
            inner.active_count = inner.active_count.saturating_sub(1);
        }
        Some(inner.event(kind, request_id))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ActivityInner> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl ActivityInner {
    fn remember_last_finished(&mut self, snapshot: RequestActivitySnapshot) {
        let is_newer = self.last_finished.as_ref().is_none_or(|current| {
            snapshot.request_id == current.request_id
                || snapshot.started_at > current.started_at
                || (snapshot.started_at == current.started_at
                    && snapshot.request_id > current.request_id)
        });
        if is_newer {
            self.last_finished = Some(snapshot);
        }
    }

    fn event(&mut self, kind: &'static str, request_id: &str) -> ActivityEvent {
        self.sequence = self.sequence.saturating_add(1);
        let mut snapshot = self
            .records
            .get(request_id)
            .map(|record| record.snapshot.clone())
            .unwrap_or_else(|| unreachable!("activity record must exist"));
        self.refresh_timings(request_id, &mut snapshot);
        let event = ActivityEvent {
            kind,
            sequence: self.sequence,
            instance_id: self.instance_id.clone(),
            active_count: self.active_count,
            snapshot,
        };
        // Publish under the same short lock that assigns the sequence.
        if let Some(events) = &self.events {
            let _ = events.send(event.clone());
        }
        event
    }

    fn refresh_timings(&self, request_id: &str, snapshot: &mut RequestActivitySnapshot) {
        let Some(record) = self.records.get(request_id) else {
            return;
        };
        let end = record.finished_instant.unwrap_or_else(Instant::now);
        if record.finished_instant.is_none() {
            snapshot.timings.total_ms = elapsed_ms(record.started_instant, end);
        }
        if record.finished_instant.is_none() {
            record.add_phase_time(&mut snapshot.timings, end);
        }
    }

    fn cleanup(&mut self, capacity: usize, ttl: Duration) {
        let now = Instant::now();
        while let Some(request_id) = self.finished_order.front() {
            let Some(record) = self.records.get(request_id) else {
                self.finished_order.pop_front();
                continue;
            };
            let Some(finished) = record.finished_instant else {
                self.finished_order.pop_front();
                continue;
            };
            if now.saturating_duration_since(finished) >= ttl {
                let request_id = self.finished_order.pop_front();
                if let Some(request_id) = request_id {
                    self.records.remove(&request_id);
                }
            } else {
                break;
            }
        }
        while self.finished_order.len() > capacity {
            let Some(request_id) = self.finished_order.pop_front() else {
                break;
            };
            self.records.remove(&request_id);
        }
    }
}

fn elapsed_ms(started: Instant, ended: Instant) -> i64 {
    ended
        .saturating_duration_since(started)
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn activity_text(value: &str) -> String {
    let mut end = value.len().min(96);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
#[path = "../../tests/unit/state/activity.rs"]
mod tests;
