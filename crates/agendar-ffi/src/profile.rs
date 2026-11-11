//! Cold-refresh timing diagnostics (`Session::cold_split`): where a cold
//! refresh spends its time. It repeats the cold refresh's pure parts outside
//! the index (read and parse every event, then resolve and reduce them in
//! memory) so the remainder of a measured refresh is the index (SQLite) and
//! snapshot work. Read-only.
#![forbid(unsafe_code)]

use std::path::Path;
use std::time::Instant;

use agendar_core::model::{Event, SourcePath, parse_event};
use agendar_core::reduce::{ReduceContext, reduce_resolved, resolve};
use agendar_core::vault::{Clock, SystemClock};

use crate::facade::FfiError;

/// Milliseconds per pure phase of a cold refresh, measured once.
#[derive(Debug, Clone, PartialEq)]
pub struct ColdSplit {
    pub events: u32,
    pub files: u32,
    pub bytes: u64,
    /// Reading every writer file and parsing its lines into events.
    pub read_parse_ms: f64,
    /// Ordering the events (`reduce::resolve`).
    pub resolve_ms: f64,
    /// Folding them into state (`reduce::reduce_resolved`).
    pub reduce_ms: f64,
}

fn millis(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// Measure the pure phases of a cold refresh of the Vault at `vault_dir`.
pub fn cold_split(vault_dir: &Path) -> Result<ColdSplit, FfiError> {
    let start = Instant::now();
    let mut events: Vec<Event> = Vec::new();
    let mut files = 0u32;
    let mut bytes = 0u64;
    for project in read_dirs(&vault_dir.join("projects"))? {
        for host in read_dirs(&project)? {
            for file in std::fs::read_dir(&host).map_err(io)? {
                let path = file.map_err(io)?.path();
                if path.extension().is_none_or(|ext| ext != "jsonl") {
                    continue;
                }
                // A source path is slash-separated on every platform.
                let relative = path
                    .strip_prefix(vault_dir)
                    .map_err(|_| core_error("outside the Vault"))?
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                let source = SourcePath::from_relative_path(&relative)?;
                let text = std::fs::read(&path).map_err(io)?;
                files += 1;
                bytes += text.len() as u64;
                for line in text.split_inclusive(|byte| *byte == b'\n') {
                    events.push(parse_event(line, &source)?);
                }
            }
        }
    }
    let read_parse_ms = millis(start);

    let start = Instant::now();
    let resolution = resolve(&events);
    let resolve_ms = millis(start);
    let start = Instant::now();
    let state = reduce_resolved(
        &resolution,
        &events,
        &ReduceContext {
            as_of: SystemClock.now(),
        },
    );
    let reduce_ms = millis(start);
    drop(state);
    Ok(ColdSplit {
        events: u32::try_from(events.len()).unwrap_or(u32::MAX),
        files,
        bytes,
        read_parse_ms,
        resolve_ms,
        reduce_ms,
    })
}

fn read_dirs(path: &Path) -> Result<Vec<std::path::PathBuf>, FfiError> {
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(path).map_err(io)? {
        let entry = entry.map_err(io)?;
        if entry.file_type().map_err(io)?.is_dir() {
            dirs.push(entry.path());
        }
    }
    Ok(dirs)
}

fn io(error: std::io::Error) -> FfiError {
    agendar_core::AgendarError::from(error).into()
}

fn core_error(message: &str) -> FfiError {
    FfiError::Core {
        code: "E_IO".to_owned(),
        detail: None,
        field: None,
        retryable: false,
        message: message.to_owned(),
    }
}
