use serde::{Deserialize, Serialize};
use sqlx::FromRow;

mod api_key;
mod catalog;
mod defaults;
mod provider;
mod route;
mod settings;
mod usage;
mod webhook;

pub(crate) use api_key::*;
pub(crate) use catalog::*;
pub(crate) use defaults::*;
pub(crate) use provider::*;
pub(crate) use route::*;
pub(crate) use settings::*;
pub(crate) use usage::*;
pub(crate) use webhook::*;

#[cfg(test)]
#[path = "../tests/unit/models.rs"]
mod capability_tests;
