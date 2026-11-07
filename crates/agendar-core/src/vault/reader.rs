//! Complete-line Vault reads.
//!
//! A byte offset always points just after a consumed LF. The reader never
//! advances over an unfinished trailing line: it may still be syncing from
//! another machine. Oversized and malformed complete lines still advance the
//! offset so one damaged line cannot block every later event.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::{AgendarError, ErrorCode, Result};

/// Maximum complete event line including the terminating LF.
pub const MAX_LINE_BYTES: usize = 65_536;

/// Maximum bytes read into one batch.
pub const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRecord {
    pub start: u64,
    pub end: u64,
    /// Complete line bytes including LF, or the bounded prefix of an oversized
    /// line (without LF).
    pub bytes: Vec<u8>,
    pub oversized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailBatch {
    pub lines: Vec<LineRecord>,
    pub next_offset: u64,
    pub observed_size: u64,
    pub trailing_partial: bool,
}

/// Read complete lines from `offset`. At most one batch budget of source bytes
/// is consumed, except that a single oversized line is always drained to its LF
/// with bounded memory, and the first line of an otherwise empty batch is always
/// returned so a caller budget smaller than one line still makes progress. An
/// ordinary line that would push the batch past its budget is left unconsumed:
/// its start stays `next_offset`.
///
/// Memory stays bounded too: the drain exception lets a line exceed the byte
/// budget, but once the budget is exhausted no further line is read, so
/// consecutive oversized lines cost at most one 64-KiB prefix per batch instead
/// of one per line until EOF.
pub fn read_complete_lines(path: &Path, offset: u64, max_bytes: usize) -> Result<TailBatch> {
    if max_bytes == 0 {
        return Err(AgendarError::validation_at(
            "/max_bytes",
            "batch_limit",
            "The read batch limit must be greater than zero.",
        ));
    }
    let budget = max_bytes.min(MAX_BATCH_BYTES);

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(source_error(
                "source_missing",
                "Cannot read Vault source: the file was not found.",
                true,
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(AgendarError::validation_at(
            "/source",
            "vault_symlink",
            "Vault source files must not be symbolic links.",
        ));
    }
    if !metadata.is_file() {
        return Err(source_error(
            "source_not_file",
            "Cannot read Vault source: it is not a regular file.",
            false,
        ));
    }

    let mut file = File::open(path)?;
    let observed_size = file.metadata()?.len();
    if offset > observed_size {
        return Err(source_error(
            "source_shrunk",
            "Cannot read Vault source: it is shorter than the recorded offset.",
            false,
        ));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file.take(observed_size - offset));

    let mut lines = Vec::new();
    let mut cursor = offset;
    let mut batch_bytes = 0_usize;
    let mut trailing_partial = false;
    loop {
        // Stop before reading another line once the batch budget is spent. An
        // oversized drain may exceed the byte budget, but its 64-KiB prefix is
        // only ever retained for one such line per batch: consecutive oversized
        // lines must not grow the batch with the file size.
        if !lines.is_empty() && batch_bytes >= budget {
            break;
        }
        match read_line_bounded(&mut reader)? {
            LineRead::Eof => break,
            LineRead::Partial => {
                trailing_partial = true;
                break;
            }
            LineRead::Complete {
                bytes,
                full_len,
                oversized,
            } => {
                let length = usize::try_from(full_len).unwrap_or(usize::MAX);
                // Stop before an ordinary line that would exceed the batch
                // budget: the budget bounds source bytes, so an ordinary line
                // is never consumed past it. Oversized lines keep their
                // explicit bounded drain exception, and a batch always makes
                // progress by consuming its first line.
                if !lines.is_empty() && !oversized && batch_bytes.saturating_add(length) > budget {
                    break;
                }
                let start = cursor;
                cursor = cursor.saturating_add(full_len);
                batch_bytes = batch_bytes.saturating_add(length);
                lines.push(LineRecord {
                    start,
                    end: cursor,
                    bytes,
                    oversized,
                });
            }
        }
    }

    Ok(TailBatch {
        lines,
        next_offset: cursor,
        observed_size,
        trailing_partial,
    })
}

enum LineRead {
    Eof,
    Partial,
    Complete {
        bytes: Vec<u8>,
        full_len: u64,
        oversized: bool,
    },
}

fn read_line_bounded<R: BufRead>(reader: &mut R) -> Result<LineRead> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut full_len = 0_u64;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if full_len == 0 {
                Ok(LineRead::Eof)
            } else {
                Ok(LineRead::Partial)
            };
        }
        let found = available.iter().position(|byte| *byte == b'\n');
        let take = found.map_or(available.len(), |index| index + 1);
        let remaining = MAX_LINE_BYTES.saturating_sub(buffer.len());
        let keep = take.min(remaining);
        buffer.extend_from_slice(&available[..keep]);
        if keep < take {
            oversized = true;
        }
        full_len = full_len.saturating_add(take as u64);
        reader.consume(take);
        if found.is_some() {
            return Ok(LineRead::Complete {
                bytes: buffer,
                full_len,
                oversized,
            });
        }
    }
}

fn source_error(detail: &str, message: &str, retryable: bool) -> AgendarError {
    AgendarError::new(ErrorCode::Io, message)
        .with_detail(detail)
        .with_retryable(retryable)
        .with_hint("Keep the Vault in place and retry; do not delete source files.")
}
