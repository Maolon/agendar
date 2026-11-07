#![forbid(unsafe_code)]

//! Portable core for Agendar protocol v1.
//!
//! The strict wire model; the append-only Vault (contained paths, locked
//! appends with crash journals, complete-line reads, verified create-only
//! blobs) and the local data-root identity/layout; the deterministic reducer
//! and rebuildable index; and the shared mutation/read service, local session
//! runtime and harness hook protocol.

pub mod adapters;
pub mod error;
pub mod index;
pub mod model;
pub mod reduce;
pub mod runtime;
pub mod service;
pub mod vault;

pub use error::{AgendarError, ErrorCode, Result};
pub use model::*;
