use super::*;

use crate::allocation::PoolDecision;

#[cfg(test)]
#[path = "../../tests/unit/scoring.rs"]
mod scoring_tests;
use crate::state::{
    MAX_SLOW_TARGET_HOLD, PROVIDER_PROBE_LEASE, SLOW_TARGET_STREAK_WINDOW, provider_open_threshold,
    slow_target_threshold_ms,
};

mod resolution;
pub(super) use resolution::*;
mod diagnostics;

pub use diagnostics::{diagnose_route, diagnose_route_for_key};
mod backoff;
pub(super) use backoff::*;
mod sinks;
pub(crate) use self::sinks::clear_target_sinks_for_route;
use sinks::clear_target_sinks_for_provider_model;
pub(super) use sinks::{
    busy_wait_delay, busy_wait_retry_after, busy_waitable_error, mark_target_sink, target_sink_key,
};
mod circuits;
pub(super) use circuits::*;
mod credentials;
pub(super) use credentials::*;
mod send;
pub(super) use send::*;
mod concurrency;
pub(super) use concurrency::acquire_upstream_slots;
use concurrency::provider_key_rpm_window;
mod affinity;
pub(crate) use affinity::SessionAffinityContext;

pub(super) use affinity::*;
mod metrics;
pub(crate) use metrics::refresh_runtime_target_p95;
use metrics::*;
mod overrides;
pub(super) use overrides::*;
mod strategies;
pub(crate) use strategies::candidate_view;

pub(super) use strategies::*;
mod assessment;

pub(crate) use assessment::*;
