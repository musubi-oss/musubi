//! Shared types for every Musubi crate: identifiers, errors, configuration.

pub mod config;
pub mod error;
pub mod id;

pub use error::{Code, Error, Result};
pub use id::*;
