//! Offset ingestion: discovery, stable single-pass reads and one projection
//! transaction per ingest.
//!
//! A source cursor is only ever committed together with the exact bytes it
//! describes: one sequential pass over the file feeds both the prefix digest and
//! the decoded lines, so a same-length rewrite between "read" and "hash" can no
//! longer acknowledge a cursor whose bytes were never decoded. Any later change
//! is detected by the recomputed prefix on the next ingest and rescanned.
//!
//! Source bytes are read before the ingest lock is taken, each read bounded by
//! [`SourceReads`]: a file a sync app has not downloaded yet never holds the
//! lock or stalls a refresh. Under the lock each source's stored cursor is
//! checked against the one its read started from before the pre-read lines
//! are committed.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use sha2::Digest as _;

use crate::error::{AgendarError, ErrorCode, Result};
use crate::index::decode::{LineOutcome, Placeholder, Redecode, decode_line_bytes};
use crate::index::source_reads::{Probe, ReadOutcome, ReadRequest, SourceRead, SourceReads};
use crate::index::{
    Index, IngestReport, StoredStatuses, UNDECODABLE, apply_resolution_changed, index_error,
    insert_placeholder, insert_raw_event, store_projection_tx, upsert_source,
};
use crate::model::State;
use crate::model::{Event, EventId, ProjectSlug, Sha256, SourcePath, UtcTime, canonical_json};
use crate::reduce::order::Resolution;
use crate::reduce::order::{VariantStatus, event_digest};
use crate::reduce::{ReduceContext, reduce_keeping_resolution};
use crate::vault::{MAX_BATCH_BYTES, MAX_LINE_BYTES, Vault, contained_path};

/// One source file cursor after scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub path: String,
    pub offset: u64,
    pub prefix_sha256: Sha256,
    pub observed_size: u64,
    pub health: String,
}

/// One observed `(source, byte range)` occurrence of a stored event variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub path: String,
    pub byte_start: u64,
    pub byte_end: u64,
    pub event_id: EventId,
    pub digest: Sha256,
}

/// A complete but undecodable or oversized line, or a source-level rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidLine {
    pub id: String,
    pub path: String,
    pub byte_start: u64,
    pub code: String,
    pub message: String,
}

/// A cryptographic cursor over every complete source prefix in the Vault.
///
/// Unlike the event-set digest, this also changes for newly arrived events that
/// do not alter a projection yet. Missing files are omitted after a refresh has
/// observed their disappearance; a live-to-missing transition still changes the
/// digest because the old live row was present in the previous cursor.
pub(crate) fn source_records_digest(records: &[SourceRecord]) -> Sha256 {
    cursor_digest(records.iter(), &[])
}

/// The cursor over source rows plus the source notes (files waiting for the
/// sync app, stray names). Without notes it is the plain row digest, so a
/// Vault with no notes keeps the cursor it always had.
fn cursor_digest<'a>(
    records: impl Iterator<Item = &'a SourceRecord>,
    notes: &[SourceNote],
) -> Sha256 {
    let mut rows: Vec<(String, u64, String, u64)> = records
        .filter(|record| record.health != "missing")
        .map(|record| {
            (
                record.path.clone(),
                record.offset,
                record.prefix_sha256.as_str().to_owned(),
                record.observed_size,
            )
        })
        .collect();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    let value = if notes.is_empty() {
        serde_json::to_value(&rows).expect("source cursor serializes")
    } else {
        let mut notes: Vec<(&str, &str)> = notes
            .iter()
            .map(|note| (note.code, note.path.as_str()))
            .collect();
        notes.sort_unstable();
        serde_json::json!({"sources": rows, "notes": notes})
    };
    Sha256::from_bytes(canonical_json(&value).as_bytes())
}

/// Recompute the source cursor without opening or locking the local index.
/// Callers use this only as a conservative freshness check for an already
/// committed projection; a mismatch or read error must not admit a write. A
/// source the sync app has not downloaded yet is a retryable error here,
/// because there is no stored cursor to stand in for it.
pub fn current_source_digest(vault: &Vault) -> Result<Sha256> {
    let scan = prescan(vault, &SourceReads::shared(), &BTreeMap::new(), &[])?;
    if let Some(path) = scan.waiting().next() {
        return Err(crate::index::source_reads::waiting_error(path));
    }
    Ok(scan.cursor())
}

/// A source the index stores a cursor for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSource {
    pub offset: u64,
    pub prefix_sha256: Sha256,
    pub observed_size: u64,
    pub health: String,
}

impl StoredSource {
    fn record(&self, path: &str) -> SourceRecord {
        SourceRecord {
            path: path.to_owned(),
            offset: self.offset,
            prefix_sha256: self.prefix_sha256.clone(),
            observed_size: self.observed_size,
            health: self.health.clone(),
        }
    }
}

/// A local health note about a Vault file: its content is not here yet
/// (`source_waiting`) or its name is not a writer file (`source_stray`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceNote {
    pub code: &'static str,
    pub path: String,
}

pub const SOURCE_WAITING: &str = "source_waiting";
pub const SOURCE_STRAY: &str = "source_stray";

impl SourceNote {
    fn message(&self) -> &'static str {
        if self.code == SOURCE_WAITING {
            "The sync app has not downloaded this file yet. Agendar shows its earlier events and reads it again on the next refresh."
        } else {
            "This file is not named like an Agendar writer file, so its events are not read. A sync app may have made it as a conflict copy."
        }
    }

    fn diagnostic(&self) -> InvalidLine {
        InvalidLine {
            id: format!("{}:{}", self.code, self.path),
            path: self.path.clone(),
            byte_start: 0,
            code: self.code.to_owned(),
            message: self.message().to_owned(),
        }
    }
}

/// How one source looked before the ingest lock.
#[derive(Debug, Clone)]
enum Seen {
    /// Read in time, starting from the stored cursor `started_from`.
    Read {
        started_from: Option<StoredSource>,
        read: Box<SourceRead>,
    },
    /// Not available in time: its stored state stands in for it.
    Waiting {
        stored: Option<StoredSource>,
    },
    Missing,
}

/// Every source read before the ingest lock, with the cursor it amounts to.
#[derive(Debug, Clone)]
pub struct Prescan {
    full_scan: bool,
    /// Every listed source, including ones skipped as links or non-files.
    listed: std::collections::BTreeSet<String>,
    sources: Vec<(SourcePath, Seen)>,
    strays: Vec<String>,
}

impl Prescan {
    /// The Vault cursor this scan observed; a waiting source counts with its
    /// stored state.
    pub fn cursor(&self) -> Sha256 {
        let mut records = Vec::new();
        for (source, seen) in &self.sources {
            match seen {
                Seen::Read { read, .. } => records.push(record_of(&source.relative_path, read)),
                Seen::Waiting {
                    stored: Some(stored),
                } => records.push(stored.record(&source.relative_path)),
                Seen::Waiting { stored: None } | Seen::Missing => {}
            }
        }
        cursor_digest(records.iter(), &self.notes())
    }

    /// Sources whose content did not arrive in time.
    pub fn waiting(&self) -> impl Iterator<Item = &str> {
        self.sources.iter().filter_map(|(source, seen)| {
            matches!(seen, Seen::Waiting { .. }).then_some(source.relative_path.as_str())
        })
    }

    /// Files in host folders whose names are not writer files.
    pub fn strays(&self) -> &[String] {
        &self.strays
    }

    fn notes(&self) -> Vec<SourceNote> {
        let mut notes: Vec<SourceNote> = self
            .waiting()
            .map(|path| SourceNote {
                code: SOURCE_WAITING,
                path: path.to_owned(),
            })
            .collect();
        notes.extend(self.strays.iter().map(|path| SourceNote {
            code: SOURCE_STRAY,
            path: path.clone(),
        }));
        notes
    }
}

fn record_of(path: &str, read: &SourceRead) -> SourceRecord {
    SourceRecord {
        path: path.to_owned(),
        offset: read.next_offset,
        prefix_sha256: read.prefix_sha256.clone(),
        observed_size: read.observed_size,
        health: if read.rewritten {
            "rewritten"
        } else if read.trailing_partial {
            "partial"
        } else {
            "ok"
        }
        .to_owned(),
    }
}

/// Read every source (or `paths`) from its stored cursor, each read bounded,
/// without any lock. `stored` is the index's source table as last committed.
pub fn prescan(
    vault: &Vault,
    reads: &SourceReads,
    stored: &BTreeMap<String, StoredSource>,
    paths: &[SourcePath],
) -> Result<Prescan> {
    let full_scan = paths.is_empty();
    let (sources, strays) = if full_scan {
        discover_with_strays(vault)?
    } else {
        (paths.to_vec(), Vec::new())
    };
    let listed = sources
        .iter()
        .map(|source| source.relative_path.clone())
        .collect();
    let mut seen: Vec<(SourcePath, Option<Seen>)> = Vec::new();
    let mut requests = Vec::new();
    let mut requested = Vec::new();
    for source in sources {
        let path = contained_path(vault.root(), &source.relative_path)?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => continue,
            Ok(metadata) => {
                let start = stored.get(&source.relative_path);
                let request = match start {
                    Some(start) if start.offset > 0 => ReadRequest {
                        start_offset: start.offset,
                        expected_prefix: Some(start.prefix_sha256.clone()),
                    },
                    _ => ReadRequest {
                        start_offset: 0,
                        expected_prefix: None,
                    },
                };
                requests.push((path, request, Probe::of(&metadata)));
                requested.push(seen.len());
                seen.push((source, None));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Only an iCloud stub is here: the content is not downloaded
                // yet, so the stored state stands in for it.
                let state = if is_icloud_stub(&icloud_stub_path(&path)) {
                    Seen::Waiting {
                        stored: stored.get(&source.relative_path).cloned(),
                    }
                } else {
                    Seen::Missing
                };
                seen.push((source, Some(state)));
            }
            Err(error) => return Err(error.into()),
        }
    }
    for (position, outcome) in requested.into_iter().zip(reads.read_all(&requests)) {
        let entry = &mut seen[position];
        let stored = stored.get(&entry.0.relative_path).cloned();
        entry.1 = Some(match outcome {
            ReadOutcome::Ready(read) => Seen::Read {
                started_from: stored,
                read,
            },
            ReadOutcome::Waiting => Seen::Waiting { stored },
            ReadOutcome::Failed(error) if error.detail() == Some("source_missing") => Seen::Missing,
            ReadOutcome::Failed(error) => return Err(error),
        });
    }
    Ok(Prescan {
        full_scan,
        listed,
        sources: seen
            .into_iter()
            .filter_map(|(source, seen)| Some((source, seen?)))
            .collect(),
        strays,
    })
}

/// The index's source table.
pub fn stored_sources(index: &Index) -> Result<BTreeMap<String, StoredSource>> {
    let mut statement = index
        .connection
        .prepare(
            "SELECT path, offset, prefix_sha256, observed_size, health FROM source_files ORDER BY path",
        )
        .map_err(index_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(index_error)?;
    let mut values = BTreeMap::new();
    for row in rows {
        let (path, offset, digest, size, health) = row.map_err(index_error)?;
        values.insert(
            path,
            StoredSource {
                offset: u64::try_from(offset).unwrap_or(0),
                prefix_sha256: Sha256::parse(&digest).unwrap_or_else(|_| empty_digest()),
                observed_size: u64::try_from(size).unwrap_or(0),
                health,
            },
        );
    }
    Ok(values)
}

/// One preflight scan of the whole Vault (used by rebuild).
pub struct Scan {
    pub events: Vec<Event>,
    pub sources: Vec<SourceRecord>,
    pub invalid_lines: Vec<InvalidLine>,
    /// Every observed event occurrence, so a rebuild can restore provenance.
    pub occurrences: Vec<Occurrence>,
    /// Every observed `(id, digest)` variant, kept placeholders included.
    pub variants: BTreeMap<(EventId, Sha256), ()>,
    /// Lines kept for a newer decoder (see `index::decode`).
    pub(crate) placeholders: Vec<Placeholder>,
    /// Envelope ids of rejected lines, best effort: such a line is still in
    /// the Vault although it has no variant.
    pub(crate) rejected_ids: std::collections::BTreeSet<EventId>,
}

/// One complete line decoded from a snapshot.
#[derive(Debug, Clone)]
pub struct DecodedLine {
    pub start: u64,
    pub end: u64,
    pub bytes: Vec<u8>,
    pub oversized: bool,
}

/// One consistent read of a source file.
#[derive(Debug, Clone)]
pub struct ConsumedSource {
    pub lines: Vec<DecodedLine>,
    /// Exclusive end of the consumed complete-line prefix.
    pub next_offset: u64,
    /// SHA-256 of `[0, next_offset)`.
    pub prefix_sha256: Sha256,
    /// SHA-256 of `[0, start_offset)` when the file still reaches it.
    pub prefix_at_start: Option<Sha256>,
    pub observed_size: u64,
    pub trailing_partial: bool,
}

/// Discover every valid writer file under `projects/`, lexical by path.
pub fn discover(vault: &Vault) -> Result<Vec<SourcePath>> {
    discover_with_strays(vault).map(|(sources, _)| sources)
}

/// The hidden stub iCloud Drive leaves for a file it has not downloaded
/// (`.name.icloud` beside `name`), on iOS and on older macOS.
fn icloud_stub_path(path: &Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.icloud"))
}

fn is_icloud_stub(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// The writer file name an iCloud stub stands for (`.name.icloud` → `name`).
fn icloud_stub_target(file_name: &str) -> Option<&str> {
    file_name
        .strip_prefix('.')?
        .strip_suffix(".icloud")
        .filter(|name| !name.is_empty() && !name.starts_with('.'))
}

/// Every valid writer file, plus every other visible file in a host folder
/// (a name that does not parse as a writer file: a conflict copy from any
/// sync app, a renamed or foreign file). Hidden files (a leading `.`) are
/// system or sync-app bookkeeping and are left alone, except an iCloud stub
/// of a writer file (`.name.icloud`): that writer file is listed, and the
/// prescan finds it waiting for its download.
pub fn discover_with_strays(vault: &Vault) -> Result<(Vec<SourcePath>, Vec<String>)> {
    let root = vault.root().join("projects");
    let mut sources = Vec::new();
    let mut strays = Vec::new();
    let projects = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((sources, strays));
        }
        Err(error) => return Err(error.into()),
    };
    for project_entry in projects {
        let project_entry = project_entry?;
        let file_type = project_entry.file_type()?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let project_name = project_entry.file_name().to_string_lossy().into_owned();
        let Ok(project) = ProjectSlug::parse(&project_name) else {
            continue;
        };
        for host_entry in std::fs::read_dir(project_entry.path())? {
            let host_entry = host_entry?;
            let file_type = host_entry.file_type()?;
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let host_name = host_entry.file_name().to_string_lossy().into_owned();
            if crate::model::HostId::parse(&host_name).is_err() {
                continue;
            }
            for file_entry in std::fs::read_dir(host_entry.path())? {
                let file_entry = file_entry?;
                let file_type = file_entry.file_type()?;
                if !file_type.is_file() || file_type.is_symlink() {
                    continue;
                }
                let file_name = file_entry.file_name().to_string_lossy().into_owned();
                let relative = format!("projects/{}/{}/{}", project.as_str(), host_name, file_name);
                match SourcePath::from_relative_path(&relative) {
                    Ok(source) => sources.push(source),
                    Err(_) if file_name.starts_with('.') => {
                        let stubbed = icloud_stub_target(&file_name).and_then(|name| {
                            SourcePath::from_relative_path(&format!(
                                "projects/{}/{}/{}",
                                project.as_str(),
                                host_name,
                                name
                            ))
                            .ok()
                        });
                        sources.extend(stubbed);
                    }
                    Err(_) => strays.push(relative),
                }
            }
        }
    }
    sources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    // A file and its stub can both be listed while a download finishes.
    sources.dedup_by(|left, right| left.relative_path == right.relative_path);
    strays.sort();
    Ok((sources, strays))
}

/// Read one source in a single sequential pass.
///
/// The digest of `[0, next_offset)` and the decoded lines come from the same
/// bytes of the same open file descriptor, so they cannot disagree. Only a
/// complete LF-terminated line is ever hashed into the committed prefix.
pub fn read_consumed(path: &Path, start_offset: u64) -> Result<ConsumedSource> {
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
    let file = std::fs::File::open(path)?;
    let mut observed_size = file.metadata()?.len();
    if start_offset > observed_size {
        return Err(source_error(
            "source_shrunk",
            "Cannot read Vault source: it is shorter than the recorded offset.",
            false,
        ));
    }
    let mut reader = std::io::BufReader::new(file);
    let mut chunk = vec![0_u8; 64 * 1024];
    let mut committed = sha2::Sha256::new();
    let mut pending = committed.clone();
    let mut prefix_at_start = (start_offset == 0).then(|| digest_of(&committed));
    let mut position = 0_u64;
    let mut line_start = 0_u64;
    let mut line_head: Vec<u8> = Vec::new();
    let mut lines: Vec<DecodedLine> = Vec::new();
    let mut decoded_bytes = 0_usize;

    'outer: loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let mut consumed = 0_usize;
        while consumed < read {
            let rest = &chunk[consumed..read];
            let finish = rest
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(rest.len(), |index| index + 1);
            let piece = &rest[..finish];
            pending.update(piece);
            let keep = MAX_LINE_BYTES
                .saturating_sub(line_head.len())
                .min(piece.len());
            line_head.extend_from_slice(&piece[..keep]);
            position += finish as u64;
            consumed += finish;
            if piece.last() != Some(&b'\n') {
                continue;
            }
            // `committed` still describes `[0, line_start)`: the already decoded
            // prefix. Record it when this line begins at the requested offset.
            if line_start == start_offset && prefix_at_start.is_none() {
                prefix_at_start = Some(digest_of(&committed));
            }
            committed = pending.clone();
            pending = committed.clone();
            let oversized = position - line_start > MAX_LINE_BYTES as u64;
            if line_start >= start_offset {
                decoded_bytes += usize::try_from(position - line_start).unwrap_or(usize::MAX);
                lines.push(DecodedLine {
                    start: line_start,
                    end: position,
                    bytes: if oversized {
                        line_head[..MAX_LINE_BYTES].to_vec()
                    } else {
                        line_head.clone()
                    },
                    oversized,
                });
            }
            line_start = position;
            line_head.clear();
            if decoded_bytes >= MAX_BATCH_BYTES {
                break 'outer;
            }
        }
    }
    let next_offset = line_start;
    // A trailing line without its LF is never consumed: its bytes must not be
    // acknowledged until a later read sees the complete line.
    let trailing_partial = position > next_offset;
    if next_offset == start_offset && prefix_at_start.is_none() {
        prefix_at_start = Some(digest_of(&committed));
    }
    if let Ok(metadata) = reader.get_ref().metadata() {
        observed_size = observed_size.max(metadata.len());
    }
    Ok(ConsumedSource {
        lines,
        next_offset,
        prefix_sha256: digest_of(&committed),
        prefix_at_start,
        observed_size,
        trailing_partial,
    })
}

/// SHA-256 of a whole file, used only for diagnostics.
pub fn hash_prefix(path: &Path, offset: u64) -> Result<Sha256> {
    let mut file = std::fs::File::open(path)?;
    let mut remaining = offset;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    while remaining > 0 {
        let read_len = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = file.read(&mut buffer[..read_len])?;
        if read == 0 {
            return Err(AgendarError::new(
                ErrorCode::Io,
                "Cannot read the recorded source prefix.",
            )
            .with_detail("source_shrunk")
            .with_retryable(true));
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(digest_of(&hasher))
}

fn digest_of(hasher: &sha2::Sha256) -> Sha256 {
    Sha256::from_digest(&hasher.clone().finalize().into())
}

/// Scan every source from offset zero and decode it into one event set.
pub fn preflight_scan(vault: &Vault) -> Result<Scan> {
    let mut scan = Scan {
        events: Vec::new(),
        sources: Vec::new(),
        invalid_lines: Vec::new(),
        occurrences: Vec::new(),
        variants: BTreeMap::new(),
        placeholders: Vec::new(),
        rejected_ids: std::collections::BTreeSet::new(),
    };
    for source in discover(vault)? {
        let path = contained_path(vault.root(), &source.relative_path)?;
        let mut offset = 0_u64;
        let (prefix_sha256, observed_size, trailing_partial) = 'scan: {
            loop {
                let snapshot = read_consumed(&path, offset)?;
                for line in &snapshot.lines {
                    match decode_line(&source, line) {
                        Decoded::Event(decoded) => {
                            let (occurrence, event) = *decoded;
                            scan.variants
                                .insert((event.id.clone(), occurrence.digest.clone()), ());
                            scan.occurrences.push(occurrence);
                            scan.events.push(event);
                        }
                        Decoded::Deferred(deferred) => {
                            let (occurrence, placeholder) = *deferred;
                            scan.variants
                                .insert((placeholder.id.clone(), placeholder.digest.clone()), ());
                            scan.occurrences.push(occurrence);
                            scan.placeholders.push(placeholder);
                        }
                        Decoded::Invalid(invalid) => {
                            if let Some(id) = crate::index::decode::envelope_id(&line.bytes) {
                                scan.rejected_ids.insert(id);
                            }
                            scan.invalid_lines.push(*invalid);
                        }
                    }
                }
                let previous = offset;
                offset = snapshot.next_offset;
                let done = snapshot.trailing_partial
                    || offset >= snapshot.observed_size
                    || offset == previous;
                if done {
                    break 'scan (
                        snapshot.prefix_sha256,
                        snapshot.observed_size,
                        snapshot.trailing_partial,
                    );
                }
            }
        };
        scan.sources.push(SourceRecord {
            path: source.relative_path.clone(),
            offset,
            prefix_sha256,
            observed_size,
            health: if trailing_partial { "partial" } else { "ok" }.to_owned(),
        });
    }
    Ok(scan)
}

/// One decoded line result.
enum Decoded {
    Event(Box<(Occurrence, Event)>),
    /// Kept for a newer decoder.
    Deferred(Box<(Occurrence, Placeholder)>),
    Invalid(Box<InvalidLine>),
}

fn decode_line(source: &SourcePath, line: &DecodedLine) -> Decoded {
    let path = source.relative_path.clone();
    if line.oversized {
        return Decoded::Invalid(Box::new(InvalidLine {
            id: format!("oversized_line:{path}:{}", line.start),
            path,
            byte_start: line.start,
            code: "oversized_line".to_owned(),
            message: "A complete event line exceeded 64 KiB and was not decoded.".to_owned(),
        }));
    }
    match decode_line_bytes(&line.bytes, source) {
        LineOutcome::Event(event) => {
            let digest = event_digest(&event);
            Decoded::Event(Box::new((
                Occurrence {
                    path,
                    byte_start: line.start,
                    byte_end: line.end,
                    event_id: event.id.clone(),
                    digest,
                },
                *event,
            )))
        }
        LineOutcome::Deferred(placeholder) => Decoded::Deferred(Box::new((
            Occurrence {
                path,
                byte_start: line.start,
                byte_end: line.end,
                event_id: placeholder.id.clone(),
                digest: placeholder.digest.clone(),
            },
            *placeholder,
        ))),
        LineOutcome::Rejected(error) => {
            Decoded::Invalid(Box::new(rejected_line(&path, line.start, &error)))
        }
    }
}

fn rejected_line(path: &str, byte_start: u64, error: &AgendarError) -> InvalidLine {
    InvalidLine {
        id: format!("invalid_line:{path}:{byte_start}"),
        path: path.to_owned(),
        byte_start,
        code: "invalid_line".to_owned(),
        message: format!(
            "A complete event line was rejected before causal analysis: {}",
            error.message()
        ),
    }
}

/// What reading the kept placeholders again with this decoder changes.
#[derive(Default)]
struct Redecoded {
    /// Occurrences that decode now, under the digest a rebuild stores.
    revived: Vec<(Occurrence, Event)>,
    /// Placeholder occurrences that now decode under another digest or are
    /// rejected for good.
    dropped: Vec<Occurrence>,
    /// Placeholder rows left without an occurrence.
    removed: Vec<(EventId, Sha256)>,
    /// Rows that now decode under their own digest.
    readable: std::collections::BTreeSet<(EventId, Sha256)>,
    /// Lines this decoder rejects for a reason no version cures.
    invalid: Vec<InvalidLine>,
}

/// A kept row's JSON and its `(path, byte_start, byte_end)` occurrences.
type KeptRow = (String, Vec<(String, u64, u64)>);

/// Decode every `undecodable` row again at each of its source occurrences,
/// exactly as a rebuild would decode that line. No source file is read.
fn redecode_stored(index: &Index) -> Result<Redecoded> {
    let mut statement = index
        .connection
        .prepare(
            "SELECT r.id, r.digest, r.json, s.path, s.byte_start, s.byte_end
             FROM raw_events r JOIN event_sources s ON s.event_id = r.id AND s.digest = r.digest
             WHERE r.reason = ?1
             ORDER BY r.id, r.digest, s.path, s.byte_start, s.byte_end",
        )
        .map_err(index_error)?;
    let rows = statement
        .query_map(rusqlite::params![UNDECODABLE], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(index_error)?;
    // `(id, digest)` -> the row's JSON and its `(path, start, end)` sources.
    let mut grouped: BTreeMap<(String, String), KeptRow> = BTreeMap::new();
    for row in rows {
        let (id, digest, json, path, start, end) = row.map_err(index_error)?;
        grouped
            .entry((id, digest))
            .or_insert((json, Vec::new()))
            .1
            .push((
                path,
                u64::try_from(start).unwrap_or(0),
                u64::try_from(end).unwrap_or(0),
            ));
    }
    let mut redecoded = Redecoded::default();
    for ((id, digest), (json, occurrences)) in grouped {
        let (Ok(id), Ok(digest)) = (EventId::parse(&id), Sha256::parse(&digest)) else {
            continue;
        };
        let mut line = json.into_bytes();
        line.push(b'\n');
        let mut attached = 0_usize;
        for (path, byte_start, byte_end) in occurrences {
            let old = Occurrence {
                path: path.clone(),
                byte_start,
                byte_end,
                event_id: id.clone(),
                digest: digest.clone(),
            };
            let Ok(source) = SourcePath::from_relative_path(&path) else {
                attached += 1;
                continue;
            };
            match decode_line_bytes(&line, &source) {
                LineOutcome::Event(event) if event.id == id => {
                    let now_digest = event_digest(&event);
                    if now_digest == digest {
                        attached += 1;
                        redecoded.readable.insert((id.clone(), digest.clone()));
                    } else {
                        redecoded.dropped.push(old);
                    }
                    redecoded.revived.push((
                        Occurrence {
                            path,
                            byte_start,
                            byte_end,
                            event_id: id.clone(),
                            digest: now_digest,
                        },
                        *event,
                    ));
                }
                LineOutcome::Event(_) | LineOutcome::Deferred(_) => attached += 1,
                LineOutcome::Rejected(error) => {
                    redecoded
                        .invalid
                        .push(rejected_line(&path, byte_start, &error));
                    redecoded.dropped.push(old);
                }
            }
        }
        if attached == 0 {
            redecoded.removed.push((id, digest));
        }
    }
    Ok(redecoded)
}

/// Sources with a rejected line whose raw bytes no index row keeps.
fn sources_with_rejected_lines(index: &Index) -> Result<Vec<String>> {
    let mut statement = index
        .connection
        .prepare(
            "SELECT DISTINCT path FROM diagnostics
             WHERE code = 'invalid_line' AND path IS NOT NULL ORDER BY path",
        )
        .map_err(index_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(index_error)?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(index_error)
}

/// What one commit folded, as a reader would reload it: the stored event set
/// in `load_variants` order, its plain resolution and the state at the
/// commit's `now`.
pub struct Committed {
    pub events: Vec<Event>,
    pub resolution: Resolution,
    pub state: State,
    pub generation: u64,
    /// The source cursor the commit stored (`None` for a partial ingest, which
    /// leaves the stored cursor as it was).
    pub source_digest: Option<Sha256>,
    /// The commit followed a decoder change and reset the cursors of the
    /// sources with rejected lines: they must be read again from the start.
    pub reread_pending: bool,
}

#[derive(Default)]
struct Batch {
    lines: Vec<(Occurrence, Event)>,
    deferred: Vec<(Occurrence, Placeholder)>,
    invalid_lines: Vec<InvalidLine>,
}

/// One ingest: read changed sources, fold at `now`, replace projections.
pub fn ingest(
    vault: &Vault,
    index: &mut Index,
    paths: &[SourcePath],
    now: UtcTime,
) -> Result<IngestReport> {
    ingest_until(vault, index, paths, now, None)
}

/// The same ingest, never waiting past `deadline` for the ingest lock.
pub fn ingest_until(
    vault: &Vault,
    index: &mut Index,
    paths: &[SourcePath],
    now: UtcTime,
    deadline: Option<Instant>,
) -> Result<IngestReport> {
    ingest_with(vault, index, &SourceReads::shared(), paths, now, deadline)
}

/// The ingest with a chosen reader: sources are read first, without the lock;
/// the lock covers only the check and the commit.
pub fn ingest_with(
    vault: &Vault,
    index: &mut Index,
    reads: &SourceReads,
    paths: &[SourcePath],
    now: UtcTime,
    deadline: Option<Instant>,
) -> Result<IngestReport> {
    let stored = stored_sources(index)?;
    let scan = prescan(vault, reads, &stored, paths)?;
    let lock = index.acquire_ingest_lock_until(vault, deadline)?;
    let first = commit_locked(vault, index, &scan, now)?;
    drop(lock);
    reread_after(vault, index, reads, now, deadline, first).map(|(report, _)| report)
}

/// The second half of a decoder change: when `first` reset source cursors,
/// read those sources again (bounded, without the lock) and commit once
/// more, so the caller's one ingest leaves the index a rebuild would. At
/// most one extra pass: the first commit stamped this decoder, so the second
/// resets nothing, and its outcome is returned whatever it reports.
fn reread_after(
    vault: &Vault,
    index: &mut Index,
    reads: &SourceReads,
    now: UtcTime,
    deadline: Option<Instant>,
    first: (IngestReport, Committed),
) -> Result<(IngestReport, Committed)> {
    if !first.1.reread_pending {
        return Ok(first);
    }
    let stored = stored_sources(index)?;
    let scan = prescan(vault, reads, &stored, &[])?;
    let _lock = index.acquire_ingest_lock_until(vault, deadline)?;
    let (mut report, committed) = commit_locked(vault, index, &scan, now)?;
    let (earlier, _) = first;
    report.added += earlier.added;
    report.changed_projects.extend(earlier.changed_projects);
    report.changed_projects.sort();
    report.changed_projects.dedup();
    let mut pulse_ids = earlier.pulse_ids;
    pulse_ids.append(&mut report.pulse_ids);
    report.pulse_ids = pulse_ids;
    Ok((report, committed))
}

/// A full ingest unless the index already stores `observed`, the Vault cursor
/// the caller computed before waiting for the lock. The second comparison runs
/// under the lock: when another process committed the same Vault state while
/// this one waited, nothing is written and the generation stays put (`None`).
/// An index without a stored cursor (written before the cursor existed) always
/// ingests, which stores one.
pub fn ingest_unless_current_until(
    vault: &Vault,
    index: &mut Index,
    observed: &Sha256,
    now: UtcTime,
    deadline: Option<Instant>,
) -> Result<Option<IngestReport>> {
    if stored_cursor(index)?.as_ref() == Some(observed) {
        return Ok(None);
    }
    let stored = stored_sources(index)?;
    let reads = SourceReads::shared();
    let scan = prescan(vault, &reads, &stored, &[])?;
    let lock = index.acquire_ingest_lock_until(vault, deadline)?;
    if stored_cursor(index)?.as_ref() == Some(observed) {
        return Ok(None);
    }
    let first = commit_locked(vault, index, &scan, now)?;
    drop(lock);
    reread_after(vault, index, &reads, now, deadline, first).map(|(report, _)| Some(report))
}

/// Commit a scan made before the lock, unless the index already stores its
/// cursor (another process committed the same Vault state first: `None`).
pub fn ingest_prescanned_until(
    vault: &Vault,
    index: &mut Index,
    scan: &Prescan,
    now: UtcTime,
    deadline: Option<Instant>,
) -> Result<Option<IngestReport>> {
    ingest_prescanned_keeping_until(vault, index, &SourceReads::shared(), scan, now, deadline)
        .map(|committed| committed.map(|(report, _)| report))
}

/// [`ingest_prescanned_until`] that also returns what the commit folded, so a
/// caller can serve it without reloading the event set from the index.
/// `reads` serves the one extra read a decoder change may need.
pub fn ingest_prescanned_keeping_until(
    vault: &Vault,
    index: &mut Index,
    reads: &SourceReads,
    scan: &Prescan,
    now: UtcTime,
    deadline: Option<Instant>,
) -> Result<Option<(IngestReport, Committed)>> {
    let lock = index.acquire_ingest_lock_until(vault, deadline)?;
    if scan.full_scan && stored_cursor(index)?.as_ref() == Some(&scan.cursor()) {
        return Ok(None);
    }
    let first = commit_locked(vault, index, scan, now)?;
    drop(lock);
    reread_after(vault, index, reads, now, deadline, first).map(Some)
}

/// The stored cursor, or `None` when this binary must commit anyway because
/// another decoder made the last commit.
fn stored_cursor(index: &Index) -> Result<Option<Sha256>> {
    crate::index::decode::current_cursor(&index.connection)
}

/// The ingest commit; the caller holds the ingest lock. No source bytes are
/// read here: a source whose stored cursor moved since its read started
/// (another process ingested it meanwhile) is skipped and keeps that cursor.
fn commit_locked(
    vault: &Vault,
    index: &mut Index,
    scan: &Prescan,
    now: UtcTime,
) -> Result<(IngestReport, Committed)> {
    let full_scan = scan.full_scan;
    let current = stored_sources(index)?;
    let mut batch = Batch::default();
    let mut records: Vec<SourceRecord> = Vec::new();
    // Cursor rows this ingest leaves as stored (waiting or moved sources).
    let mut kept: Vec<SourceRecord> = Vec::new();
    // Sources read from the beginning in this batch: their path-bearing
    // diagnostics describe the old bytes and must be regenerated.
    let mut rescanned: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Recorded sources that are no longer discoverable keep their cache and are
    // reported as pending file health rather than being silently forgotten.
    if full_scan {
        for (path, row) in &current {
            if scan.listed.contains(path) {
                continue;
            }
            let file = contained_path(vault.root(), path)?;
            if std::fs::symlink_metadata(&file).is_err() {
                let mut record = row.record(path);
                record.health = "missing".to_owned();
                records.push(record);
            }
        }
    }

    for (source, seen) in &scan.sources {
        let relative = &source.relative_path;
        let now_stored = current.get(relative);
        match seen {
            Seen::Missing => records.push(SourceRecord {
                path: relative.clone(),
                offset: now_stored.map_or(0, |row| row.offset),
                prefix_sha256: now_stored
                    .map_or_else(empty_digest, |row| row.prefix_sha256.clone()),
                observed_size: now_stored.map_or(0, |row| row.observed_size),
                health: "missing".to_owned(),
            }),
            Seen::Waiting { .. } => {
                if let Some(row) = now_stored {
                    kept.push(row.record(relative));
                }
            }
            Seen::Read { started_from, read } => {
                if now_stored != started_from.as_ref() {
                    if let Some(row) = now_stored {
                        kept.push(row.record(relative));
                    }
                    continue;
                }
                // A read from offset zero (a new source, or one whose cursor a
                // re-decode reset) regenerates every line diagnostic.
                if started_from.as_ref().is_none_or(|row| row.offset == 0) || read.rewritten {
                    rescanned.insert(relative.clone());
                }
                if read.rewritten {
                    batch.invalid_lines.push(rewrite_line(relative));
                }
                for line in &read.lines {
                    match decode_line(source, line) {
                        Decoded::Event(decoded) => batch.lines.push(*decoded),
                        Decoded::Deferred(deferred) => batch.deferred.push(*deferred),
                        Decoded::Invalid(invalid) => batch.invalid_lines.push(*invalid),
                    }
                }
                records.push(record_of(relative, read));
            }
        }
    }
    let notes = scan.notes();

    // Fold the committed event set plus this batch before opening the write
    // transaction, so the projection is one consistent snapshot.
    // The folded set is kept in `load_variants` order (`ORDER BY id, digest`,
    // byte order), so it is exactly the event list a reader reloads after
    // this commit.
    // Another decoder made the last commit: read its placeholders again (no
    // source read). Lines it rejected outright left no raw bytes, and this
    // decoder may accept them, so the sources with rejected lines are read
    // again from the start by the caller's second pass (`reread_after`).
    let due = crate::index::decode::redecode_due(&index.connection)?;
    let redecoded = if due == Redecode::Current {
        Redecoded::default()
    } else {
        redecode_stored(index)?
    };
    let reread = if due == Redecode::Current {
        Vec::new()
    } else {
        sources_with_rejected_lines(index)?
    };
    let stored = crate::index::load_variants(index)?;
    let mut undecodable: Vec<(EventId, Sha256)> = Vec::new();
    let mut keyed: Vec<(Sha256, Event)> = Vec::with_capacity(stored.len() + batch.lines.len());
    // Stored rows whose status already matches the new resolution are not
    // rewritten. A batch row is new (`insert_raw_event` skips an existing
    // one), so it has no stored status here and is always written.
    let mut known: StoredStatuses = BTreeMap::new();
    for variant in stored {
        match variant.event {
            Some(event) => {
                known.insert(
                    (event.id.clone(), variant.digest.clone()),
                    Some((variant.status, variant.reason)),
                );
                keyed.push((variant.digest, event));
            }
            None => {
                let key = (variant.id, variant.digest);
                if !redecoded.readable.contains(&key) && !redecoded.removed.contains(&key) {
                    undecodable.push(key);
                }
            }
        }
    }
    for (occurrence, event) in batch.lines.iter().chain(&redecoded.revived) {
        // The row is stored under the occurrence's digest (`insert_raw_event`).
        let digest = occurrence.digest.clone();
        if let std::collections::btree_map::Entry::Vacant(entry) =
            known.entry((event.id.clone(), digest.clone()))
        {
            entry.insert(None);
            keyed.push((digest, event.clone()));
        }
    }
    keyed.sort_by(|(left_digest, left), (right_digest, right)| {
        (left.id.as_str(), left_digest.as_str()).cmp(&(right.id.as_str(), right_digest.as_str()))
    });
    let fold_events: Vec<Event> = keyed.into_iter().map(|(_, event)| event).collect();
    let output = reduce_keeping_resolution(&fold_events, &ReduceContext { as_of: now });
    let resolution = output.last();
    let state = &output.state;
    let generation = crate::index::meta(&index.connection, "generation")?
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_add(1);
    let old_projects = project_json_map(index)?;
    let mut changed_projects: Vec<ProjectSlug> = Vec::new();
    for (slug, project) in &state.projects {
        let previous = old_projects.get(slug.as_str());
        let next = serde_json::to_string(&project.summary).expect("summary serializes");
        if previous.map(String::as_str) != Some(next.as_str()) {
            changed_projects.push(slug.clone());
        }
    }
    changed_projects.sort();
    let mut pulse_ids: Vec<(String, EventId)> = Vec::new();
    for (_, event) in &batch.lines {
        if event.kind() == "pulse" && resolution.order_index.contains_key(&event.id) {
            pulse_ids.push((event.ts.canonical(), event.id.clone()));
        }
    }
    pulse_ids.sort();
    let pending = resolution
        .variants
        .values()
        .filter(|variant| variant.status == VariantStatus::Pending)
        .count() as u64;
    let invalid = resolution
        .variants
        .values()
        .filter(|variant| variant.status == VariantStatus::Quarantined)
        .count() as u64;

    let transaction = index
        .connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(index_error)?;
    let mut added = 0_u64;
    for record in &records {
        upsert_source(&transaction, record, now)?;
    }
    if full_scan {
        transaction
            .execute(
                "DELETE FROM diagnostics WHERE code IN (?1, ?2)",
                rusqlite::params![SOURCE_WAITING, SOURCE_STRAY],
            )
            .map_err(index_error)?;
    }
    for path in &rescanned {
        transaction
            .execute(
                "DELETE FROM diagnostics WHERE path = ?1 AND event_id IS NULL",
                rusqlite::params![path],
            )
            .map_err(index_error)?;
    }
    for (occurrence, event) in &batch.lines {
        if insert_raw_event(&transaction, event, &occurrence.digest)? {
            added += 1;
        }
        insert_occurrence(&transaction, occurrence)?;
    }
    for (occurrence, placeholder) in &batch.deferred {
        insert_placeholder(&transaction, placeholder)?;
        insert_occurrence(&transaction, occurrence)?;
    }
    for (occurrence, event) in &redecoded.revived {
        insert_raw_event(&transaction, event, &occurrence.digest)?;
        insert_occurrence(&transaction, occurrence)?;
    }
    for occurrence in &redecoded.dropped {
        transaction
            .execute(
                "DELETE FROM event_sources WHERE path = ?1 AND byte_start = ?2 AND byte_end = ?3
                 AND event_id = ?4 AND digest = ?5",
                rusqlite::params![
                    occurrence.path,
                    i64::try_from(occurrence.byte_start).unwrap_or(i64::MAX),
                    i64::try_from(occurrence.byte_end).unwrap_or(i64::MAX),
                    occurrence.event_id.as_str(),
                    occurrence.digest.as_str()
                ],
            )
            .map_err(index_error)?;
    }
    for (id, digest) in &redecoded.removed {
        transaction
            .execute(
                "DELETE FROM raw_events WHERE id = ?1 AND digest = ?2 AND NOT EXISTS
                 (SELECT 1 FROM event_sources WHERE event_id = ?1 AND digest = ?2)",
                rusqlite::params![id.as_str(), digest.as_str()],
            )
            .map_err(index_error)?;
    }
    for invalid_line in batch.invalid_lines.iter().chain(&redecoded.invalid) {
        crate::index::insert_invalid_line(&transaction, invalid_line, now)?;
    }
    if full_scan {
        for note in &notes {
            crate::index::insert_invalid_line(&transaction, &note.diagnostic(), now)?;
        }
    }
    for (id, digest) in &undecodable {
        transaction
            .execute(
                "UPDATE raw_events SET status = 'quarantined', reason = 'undecodable'
                 WHERE id = ?1 AND digest = ?2",
                rusqlite::params![id.as_str(), digest.as_str()],
            )
            .map_err(index_error)?;
    }
    apply_resolution_changed(&transaction, resolution, &known)?;
    crate::index::clear_projections(&transaction)?;
    transaction
        .execute("DELETE FROM diagnostics WHERE path IS NULL", [])
        .map_err(index_error)?;
    store_projection_tx(&transaction, state, generation, now)?;
    for path in &reread {
        // Read from the start next time; without a stored cursor the next
        // refresh cannot take the unchanged fast path.
        transaction
            .execute(
                "UPDATE source_files SET offset = 0, prefix_sha256 = ?2, observed_size = 0
                 WHERE path = ?1",
                rusqlite::params![path, empty_digest().as_str()],
            )
            .map_err(index_error)?;
    }
    let source_digest = (full_scan && reread.is_empty())
        .then(|| cursor_digest(records.iter().chain(&kept), &notes));
    if let Some(cursor) = &source_digest {
        crate::index::set_meta(&transaction, "source_digest", cursor.as_str())?;
    } else if !reread.is_empty() {
        transaction
            .execute("DELETE FROM meta WHERE key = 'source_digest'", [])
            .map_err(index_error)?;
    }
    transaction.commit().map_err(index_error)?;

    let report = IngestReport {
        added,
        pending,
        invalid,
        generation,
        changed_projects,
        pulse_ids: pulse_ids.into_iter().map(|(_, id)| id).collect(),
    };
    let committed = Committed {
        events: fold_events,
        resolution: output.resolved,
        state: output.state,
        generation,
        source_digest,
        reread_pending: !reread.is_empty(),
    };
    Ok((report, committed))
}

fn rewrite_line(path: &str) -> InvalidLine {
    InvalidLine {
        id: format!("source_rewritten:{path}"),
        path: path.to_owned(),
        byte_start: 0,
        code: "source_rewritten".to_owned(),
        message: "The committed prefix changed or the source shrank; it is rescanned from the beginning while earlier observed events are retained."
            .to_owned(),
    }
}

pub(crate) fn insert_occurrence(
    transaction: &rusqlite::Transaction<'_>,
    occurrence: &Occurrence,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO event_sources(path, byte_start, byte_end, event_id, digest)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path, byte_start, byte_end, event_id, digest) DO NOTHING",
            rusqlite::params![
                occurrence.path,
                i64::try_from(occurrence.byte_start).unwrap_or(i64::MAX),
                i64::try_from(occurrence.byte_end).unwrap_or(i64::MAX),
                occurrence.event_id.as_str(),
                occurrence.digest.as_str()
            ],
        )
        .map_err(index_error)?;
    Ok(())
}

fn project_json_map(index: &Index) -> Result<BTreeMap<String, String>> {
    let mut statement = index
        .connection
        .prepare("SELECT slug, json FROM projects")
        .map_err(index_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(index_error)?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (slug, json) = row.map_err(index_error)?;
        map.insert(slug, json);
    }
    Ok(map)
}

fn source_error(detail: &str, message: &str, retryable: bool) -> AgendarError {
    AgendarError::new(ErrorCode::Io, message)
        .with_detail(detail)
        .with_retryable(retryable)
}

fn empty_digest() -> Sha256 {
    Sha256::from_bytes(b"")
}
