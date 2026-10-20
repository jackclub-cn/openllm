//! Lifecycle APIs: configuration export/import, database backups, the schema
//! pre-check and the upgrade check.
//!
//! Everything in this module is admin-only and is meant to make a gateway
//! reproducible: a configuration document you can keep in git, a consistent
//! backup you can restore into an empty data directory, a report of the
//! migrations a start would run, and a version comparison.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

use super::*;

#[cfg(test)]
#[path = "../../tests/unit/lifecycle.rs"]
mod tests;

mod export;
pub use export::*;
mod importer;
pub use importer::*;
mod comparison;
use comparison::*;
mod backups;
pub use backups::*;
mod upgrade;
pub use upgrade::*;
