//! Vault protocol v1: contained paths, locked appends, complete-line reads and
//! verified create-only blobs.
//!
//! The Vault is an append-only event tree. Every mutation validates the full
//! event with the model decoder (`parse_event`) before anything is
//! journalled or written, and every local lock, journal and temporary file
//! lives outside the Vault under the selected data root.
//!
//! This module only declares its siblings and re-exports the public API:
//! `paths` owns the Vault root, manifest and contained paths, `writer` owns the
//! append/seal protocol and its clocks, `reader` owns complete-line reads and
//! `blobs` owns verified create-only publication. `listing` only reads file
//! names and sizes for the usage report.

pub mod blobs;
pub mod listing;
pub mod paths;
pub mod reader;
pub mod writer;

pub use crate::error::Result;
pub use crate::model::{SourcePath, parse_event};
pub use crate::runtime::LocalPaths;

pub use blobs::{blob_journal_path, blob_lock_path, put_blob, read_blob};
pub use paths::{
    Vault, VaultManifest, blob_relative_path, contained_path, contains_vault_manifest,
    ensure_directory, ensure_host_directory, host_relative_path, writer_file_name,
    writer_relative_path,
};
pub use reader::{LineRecord, MAX_BATCH_BYTES, MAX_LINE_BYTES, TailBatch, read_complete_lines};
pub use writer::{
    AppendResult, Clock, EventDraft, Following, IdSource, SEGMENT_SEAL_THRESHOLD, SealReason,
    SystemClock, UlidIdSource, WriterKey, append_event, append_event_reserved,
    append_events_reserved, recover_writer, seal_writer, segment_would_seal, segment_write_calls,
    writer_journal_path, writer_lock_path,
};
