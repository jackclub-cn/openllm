use super::*;
use std::sync::atomic::Ordering::Relaxed;

#[cfg(test)]
#[path = "../../tests/unit/diagnostics.rs"]
mod tests;

mod checks;
pub(crate) use checks::run_self_check;

pub use checks::*;
mod startup_checks;
use startup_checks::*;
mod provider_checks;
use provider_checks::*;
mod routing_checks;
use routing_checks::*;
mod runtime_checks;
use runtime_checks::*;
mod snapshot;
use snapshot::is_secret_header;
pub(crate) use snapshot::redact_headers;
pub use snapshot::*;
