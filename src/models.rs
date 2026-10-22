use serde::{Deserialize, Serialize};
use sqlx::FromRow;

mod api_key;
mod audit;
mod catalog;
mod context;
mod defaults;
mod lifecycle;
mod pool;
mod provider;
mod quota;
mod route;
mod settings;
mod usage;

pub(crate) use api_key::*;
pub(crate) use audit::*;
pub(crate) use catalog::*;
pub(crate) use context::*;
pub(crate) use defaults::*;
pub(crate) use lifecycle::*;
pub(crate) use pool::*;
pub(crate) use provider::*;
pub(crate) use quota::*;
pub(crate) use route::*;
pub(crate) use settings::*;
pub(crate) use usage::*;

#[cfg(test)]
#[path = "../tests/unit/models.rs"]
mod capability_tests;
