//! Offset ingestion: discovery, stable single-pass reads and one projection
//! transaction per ingest.
//!
//! A source cursor is only ever committed together with the exact bytes it
//! describes: one sequential pass over the file feeds both the prefix digest and
//! the decoded lines, so a same-length rewrite between "read" and "hash" can no
//! longer acknowledge a cursor whose bytes were never decoded. Any later change
//! is detected by the recomputed prefix on the next ingest and rescanned.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

use sha2::Digest as _;

use crate::error::{AgendarError, ErrorCode, Result};
use crate::index::{
    Index, IngestReport, apply_resolution, index_error, insert_raw_event, store_projection_tx,
    upsert_source,
};
use crate::model::{Event, EventId, ProjectSlug, Sha256, SourcePath, UtcTime, parse_event};
use crate::reduce::order::{VariantStatus, event_digest};
use crate::reduce::{ReduceContext, reduce_with_resolution};
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

/// One preflight scan of the whole Vault (used by rebuild).
pub struct Scan {
    pub events: Vec<Event>,
    pub sources: Vec<SourceRecord>,
    pub invalid_lines: Vec<InvalidLine>,
    /// Every observed event occurrence, so a rebuild can restore provenance.
    pub occurrences: Vec<Occurrence>,
    /// Every observed `(id, digest)` variant.
    pub variants: BTreeMap<(EventId, Sha256), ()>,
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
    let root = vault.root().join("projects");
    let mut sources = Vec::new();
    let projects = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(sources),
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
                if let Ok(source) = SourcePath::from_relative_path(&relative) {
                    sources.push(source);
                }
            }
        }
    }
    sources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(sources)
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
                        Decoded::Invalid(invalid) => scan.invalid_lines.push(*invalid),
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
    match parse_event(&line.bytes, source) {
        Ok(event) => {
            let digest = event_digest(&event);
            Decoded::Event(Box::new((
                Occurrence {
                    path,
                    byte_start: line.start,
                    byte_end: line.end,
                    event_id: event.id.clone(),
                    digest,
                },
                event,
            )))
        }
        Err(error) => Decoded::Invalid(Box::new(InvalidLine {
            id: format!("invalid_line:{path}:{}", line.start),
            path,
            byte_start: line.start,
            code: "invalid_line".to_owned(),
            message: format!(
                "A complete event line was rejected before causal analysis: {}",
                error.message()
            ),
        })),
    }
}

#[derive(Default)]
struct Batch {
    lines: Vec<(Occurrence, Event)>,
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
    let _lock = index.acquire_ingest_lock_until(vault, deadline)?;
    let sources = if paths.is_empty() {
        discover(vault)?
    } else {
        paths.to_vec()
    };
    let mut batch = Batch::default();
    let mut records: Vec<SourceRecord> = Vec::new();
    // Sources read from the beginning in this batch: their path-bearing
    // diagnostics describe the old bytes and must be regenerated.
    let mut rescanned: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Recorded sources that are no longer discoverable keep their cache and are
    // reported as pending file health rather than being silently forgotten.
    let scanned: std::collections::BTreeSet<String> = sources
        .iter()
        .map(|source| source.relative_path.clone())
        .collect();
    for row in all_source_rows(index)? {
        if scanned.contains(&row.0) {
            continue;
        }
        let path = contained_path(vault.root(), &row.0)?;
        if std::fs::symlink_metadata(&path).is_err() {
            records.push(SourceRecord {
                path: row.0,
                offset: row.1,
                prefix_sha256: row.2,
                observed_size: row.3,
                health: "missing".to_owned(),
            });
        }
    }

    for source in &sources {
        let path = contained_path(vault.root(), &source.relative_path)?;
        let existing = source_row(index, &source.relative_path)?;
        let mut offset = existing.as_ref().map(|row| row.0).unwrap_or(0);
        let recorded_digest = existing.as_ref().map(|row| row.1.clone());
        let mut verified = offset == 0;
        let mut health = "ok";
        let mut rewrite_reported = false;
        if existing.is_none() {
            rescanned.insert(source.relative_path.clone());
        }
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => continue,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                records.push(SourceRecord {
                    path: source.relative_path.clone(),
                    offset,
                    prefix_sha256: recorded_digest.unwrap_or_else(empty_digest),
                    observed_size: existing.as_ref().map(|row| row.2).unwrap_or(0),
                    health: "missing".to_owned(),
                });
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let mut state: (Sha256, u64, bool);
        loop {
            let snapshot = match read_consumed(&path, offset) {
                Ok(snapshot) => snapshot,
                Err(error) if error.detail() == Some("source_shrunk") => {
                    if offset == 0 {
                        return Err(error);
                    }
                    offset = 0;
                    health = "rewritten";
                    rescanned.insert(source.relative_path.clone());
                    if !rewrite_reported {
                        rewrite_reported = true;
                        batch
                            .invalid_lines
                            .push(rewrite_line(&source.relative_path));
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            if !verified {
                verified = true;
                if let Some(recorded) = &recorded_digest
                    && snapshot.prefix_at_start.as_ref() != Some(recorded)
                {
                    // The committed prefix changed: rescan complete content while
                    // earlier observed events stay in the cache.
                    offset = 0;
                    health = "rewritten";
                    rescanned.insert(source.relative_path.clone());
                    if !rewrite_reported {
                        rewrite_reported = true;
                        batch
                            .invalid_lines
                            .push(rewrite_line(&source.relative_path));
                    }
                    continue;
                }
            }
            for line in &snapshot.lines {
                match decode_line(source, line) {
                    Decoded::Event(decoded) => batch.lines.push(*decoded),
                    Decoded::Invalid(invalid) => batch.invalid_lines.push(*invalid),
                }
            }
            let previous = offset;
            offset = snapshot.next_offset;
            let done =
                snapshot.trailing_partial || offset >= snapshot.observed_size || offset == previous;
            state = (
                snapshot.prefix_sha256,
                snapshot.observed_size,
                snapshot.trailing_partial,
            );
            if done {
                break;
            }
        }
        let (prefix_sha256, observed_size, trailing_partial) = state;
        records.push(SourceRecord {
            path: source.relative_path.clone(),
            offset,
            prefix_sha256,
            observed_size,
            health: if health == "rewritten" {
                "rewritten".to_owned()
            } else if trailing_partial {
                "partial".to_owned()
            } else {
                "ok".to_owned()
            },
        });
    }

    // Fold the committed event set plus this batch before opening the write
    // transaction, so the projection is one consistent snapshot.
    let stored = crate::index::load_variants(index)?;
    let mut undecodable: Vec<(EventId, Sha256)> = Vec::new();
    let mut fold_events: Vec<Event> = Vec::new();
    let mut known: BTreeMap<(EventId, Sha256), ()> = BTreeMap::new();
    for variant in &stored {
        match &variant.event {
            Some(event) => {
                known.insert((event.id.clone(), variant.digest.clone()), ());
                fold_events.push(event.clone());
            }
            None => undecodable.push((variant.id.clone(), variant.digest.clone())),
        }
    }
    for (_, event) in &batch.lines {
        let digest = event_digest(event);
        if known.insert((event.id.clone(), digest), ()).is_none() {
            fold_events.push(event.clone());
        }
    }
    let output = reduce_with_resolution(&fold_events, &ReduceContext { as_of: now }, None);
    let resolution = output.resolution;
    let state = output.state;
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
    for invalid_line in &batch.invalid_lines {
        crate::index::insert_invalid_line(&transaction, invalid_line, now)?;
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
    apply_resolution(&transaction, &resolution)?;
    crate::index::clear_projections(&transaction)?;
    transaction
        .execute("DELETE FROM diagnostics WHERE path IS NULL", [])
        .map_err(index_error)?;
    store_projection_tx(&transaction, &state, generation, now)?;
    transaction.commit().map_err(index_error)?;

    Ok(IngestReport {
        added,
        pending,
        invalid,
        generation,
        changed_projects,
        pulse_ids: pulse_ids.into_iter().map(|(_, id)| id).collect(),
    })
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

fn source_row(index: &Index, path: &str) -> Result<Option<(u64, Sha256, u64)>> {
    use rusqlite::OptionalExtension as _;
    let row = index
        .connection
        .query_row(
            "SELECT offset, prefix_sha256, observed_size FROM source_files WHERE path = ?1",
            rusqlite::params![path],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(index_error)?;
    Ok(row.map(|(offset, digest, size)| {
        (
            u64::try_from(offset).unwrap_or(0),
            Sha256::parse(&digest).unwrap_or_else(|_| empty_digest()),
            u64::try_from(size).unwrap_or(0),
        )
    }))
}

fn all_source_rows(index: &Index) -> Result<Vec<(String, u64, Sha256, u64)>> {
    let mut statement = index
        .connection
        .prepare(
            "SELECT path, offset, prefix_sha256, observed_size FROM source_files ORDER BY path",
        )
        .map_err(index_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(index_error)?;
    let mut values = Vec::new();
    for row in rows {
        let (path, offset, digest, size) = row.map_err(index_error)?;
        values.push((
            path,
            u64::try_from(offset).unwrap_or(0),
            Sha256::parse(&digest).unwrap_or_else(|_| empty_digest()),
            u64::try_from(size).unwrap_or(0),
        ));
    }
    Ok(values)
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
