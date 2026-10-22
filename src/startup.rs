//! Startup safety: single-writer protection and pre-migration backups.
//!
//! The gateway stores everything in one SQLite file and is designed as a single
//! instance (SQLite serialises writers, so two processes on the same file do
//! not fail loudly — they corrupt each other's expectations, for example by
//! each keeping its own in-memory cooldowns and pool windows). The schema
//! comment in the README said "do not run two instances"; this module turns
//! that note into a startup check that names the other instance instead of
//! leaving an operator to discover it through conflicting runtime state.
//!
//! Two mechanisms live here:
//!
//! * a **lease** row per running instance, refreshed by a heartbeat and
//!   expiring on a TTL. A fresh foreign lease makes the new process refuse to
//!   start unless the operator explicitly allows it; a lease left behind by a
//!   crashed process on the same host is taken over after checking that its pid
//!   is gone.
//! * a **pre-migration backup**: when a newer binary has pending migrations and
//!   the database already holds data, a consistent `VACUUM INTO` copy is written
//!   next to it first. A failed migration then aborts startup with the backup
//!   path in the error instead of leaving an operator to recover by hand.

use std::path::{Path, PathBuf};

use anyhow::Context;
use sqlx::SqlitePool;

#[cfg(test)]
#[path = "../tests/unit/startup.rs"]
mod tests;

mod lease;
pub(crate) use lease::*;
mod migrations;

pub(crate) use migrations::*;
mod backups;

pub(crate) use backups::*;
mod verification;
pub(crate) use verification::*;
mod restore;
pub(crate) use restore::*;
