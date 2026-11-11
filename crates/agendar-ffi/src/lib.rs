//! UniFFI facade over `agendar-core` for mobile clients.
//!
//! The crate is the third `unsafe` exception (AGENTS.md), limited to the code
//! UniFFI generates: `scaffolding.rs` includes it and is the only module that
//! allows `unsafe_code`. Every hand-written module forbids it, and
//! `tests/no_unsafe.rs` fails if the `unsafe` keyword appears anywhere in
//! `src/` or a second module allows `unsafe_code`.
#![deny(unsafe_code)]

mod facade;
#[allow(unsafe_code)]
mod scaffolding;
// Cold-refresh timing diagnostics (`Session::cold_split`).
mod profile;

// The generated glue names its type tag from the crate root.
use scaffolding::UniFfiTag;

pub use facade::*;
