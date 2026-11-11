//! Locked, crash-recoverable writer appends and explicit sealing.
//!
//! One local writer lock and one local journal exist per `(Vault, project,
//! writer)`. The journal record is the five fields;
//! the exact pending line bytes are stored beside it and referenced by digest,
//! so a partially written suffix can be completed without guessing and without
//! ever truncating a Vault file.
//!
//! Recovery refuses to guess: bytes beyond the journalled suffix, a suffix that
//! conflicts with the journalled bytes and unverifiable committed pending bytes
//! all freeze the writer instead of admitting a new draft. A readable suffix is
//! flushed to durable storage before the recovery material is cleared.

use std::cell::Cell;
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::WriterId;
use crate::model::{
    Event, EventId, EventPayload, ProjectSlug, Refs, Sha256, SourcePath, UtcTime, canonical_json,
    parse_event,
};
use crate::runtime::LocalPaths;
use crate::runtime::local_state::{
    acquire_lock, read_json_value, sync_dir, write_bytes_atomic, write_json_atomic,
};
use crate::vault::paths::{
    Vault, contained_path, ensure_directory, ensure_host_directory, last_byte_is_lf,
    writer_file_name, writer_relative_path,
};
use crate::vault::reader::{MAX_LINE_BYTES, read_complete_lines};

/// Segment seal threshold in bytes (1 MiB).
pub const SEGMENT_SEAL_THRESHOLD: u64 = 1_048_576;

/// Largest JavaScript-safe sequence number.
pub const MAX_SAFE_SEQ: u64 = 9_007_199_254_740_991;

const JOURNAL_VERSION: u8 = 1;
const ID_ATTEMPTS: usize = 16;

/// Clock used by I/O callers. The reducer never reads it.
pub trait Clock {
    fn now(&self) -> UtcTime;
}

/// Event-ID generator used by I/O callers. The model decoder never generates
/// IDs.
pub trait IdSource {
    fn next_event_id(&mut self) -> EventId;
}

/// Wall clock for CLI, steward and app processes.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UtcTime {
        let now = chrono::Utc::now();
        let millis = chrono::DateTime::from_timestamp_millis(now.timestamp_millis())
            .expect("the system clock is inside the supported 1970..=9999 range");
        UtcTime::from_datetime(millis).expect("the wall clock has millisecond precision")
    }
}

/// Fresh ULID event IDs, generated only at the I/O boundary.
#[derive(Debug, Default, Clone, Copy)]
pub struct UlidIdSource;

impl IdSource for UlidIdSource {
    fn next_event_id(&mut self) -> EventId {
        EventId::parse(&ulid::Ulid::generate().to_string())
            .expect("a generated ULID is a canonical EventId")
    }
}

pub(crate) fn vault_io_error(operation: &str, error: std::io::Error) -> AgendarError {
    AgendarError::new(
        ErrorCode::Io,
        format!("Cannot {operation}: {}.", safe_os_reason(&error)),
    )
    .with_detail("vault_io")
    .with_hint("Check permissions and Vault availability; retry without deleting logs.")
}

fn safe_os_reason(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "the file was not found".to_owned(),
        std::io::ErrorKind::PermissionDenied => "permission was denied".to_owned(),
        std::io::ErrorKind::AlreadyExists => "the file already exists".to_owned(),
        std::io::ErrorKind::StorageFull => "the disk is full".to_owned(),
        std::io::ErrorKind::ReadOnlyFilesystem => "the filesystem is read-only".to_owned(),
        _ => "the operating system refused the request".to_owned(),
    }
}

/// Logical writer stream inside one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterKey {
    pub project: ProjectSlug,
    pub writer: WriterId,
}

/// Caller-supplied event content. Envelope identity, sequence, timestamp,
/// writer and project are assigned by the Vault.
///
/// The draft is serializable so a multi-event service operation can persist its
/// immutable payload before appending. Serializing a draft never persists the
/// generated event envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventDraft {
    pub refs: Refs,
    pub payload: EventPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendResult {
    pub event: Event,
    pub relative_path: String,
    pub byte_end: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SealReason {
    Size,
    SessionEnded,
}

/// The durable pending record The serialized line
/// bytes live beside the journal and must match `bytes` and `digest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingAppend {
    pub path: String,
    pub offset: u64,
    pub bytes: u64,
    pub digest: Sha256,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AbandonedTail {
    path: String,
    offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriterJournal {
    v: u8,
    project: ProjectSlug,
    writer: WriterId,
    #[serde(default)]
    next_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<PendingAppend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sealed: Option<SealReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sealed_segment: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    forked: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    abandoned: Vec<AbandonedTail>,
}

impl WriterJournal {
    fn new(key: &WriterKey) -> Self {
        Self {
            v: JOURNAL_VERSION,
            project: key.project.clone(),
            writer: key.writer.clone(),
            next_seq: 0,
            pending: None,
            sealed: None,
            sealed_segment: None,
            forked: None,
            abandoned: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct SegmentInfo {
    segment: u32,
    relative: String,
    size: u64,
}

#[derive(Debug, Clone, Default)]
struct Scan {
    max_seq: u64,
    ids: BTreeMap<EventId, Sha256>,
    segments: Vec<SegmentInfo>,
    max_segment: Option<u32>,
    last_size: u64,
    partial_tail: bool,
    partial_offset: Option<u64>,
}

/// The events of one completed pending append, with each event's exact line.
struct Recovered {
    results: Vec<AppendResult>,
    lines: Vec<Vec<u8>>,
}

/// The drafts that follow the first event of one append, each with its
/// optionally reserved ID. They are built from the first event's ID so a
/// follower can cite it.
pub type Following<'a> = &'a dyn Fn(&EventId) -> Vec<(EventDraft, Option<EventId>)>;

enum Slot {
    Existing(AppendResult),
    New { event: Event, line: String },
}

thread_local! {
    static SEGMENT_WRITES: Cell<u64> = const { Cell::new(0) };
}

/// How many write calls this thread has made to writer segments. Tests count
/// appends with it; nothing reads it to decide anything.
#[doc(hidden)]
pub fn segment_write_calls() -> u64 {
    SEGMENT_WRITES.with(Cell::get)
}

fn count_segment_write() {
    SEGMENT_WRITES.with(|count| count.set(count.get().saturating_add(1)));
}

/// Whether appending `line_len` bytes to a nonempty segment would exceed the
/// seal threshold. An exact fit stays in the current segment.
pub const fn segment_would_seal(current_size: u64, line_len: u64) -> bool {
    current_size > 0 && current_size.saturating_add(line_len) > SEGMENT_SEAL_THRESHOLD
}

/// Validate and append one event, recovering a crashed append first.
///
/// If the pending journal already holds a fully durable event whose content
/// matches `draft`, that original event is returned instead of writing a
/// duplicate. A different draft is appended after the previous append is
/// completed.
pub fn append_event(
    vault: &Vault,
    local: &LocalPaths,
    key: &WriterKey,
    draft: EventDraft,
    clock: &dyn crate::vault::Clock,
    ids: &mut dyn crate::vault::IdSource,
) -> Result<AppendResult> {
    append_event_reserved(vault, local, key, draft, None, clock, ids)
}

/// Append one event with an optionally reserved ID (see [`append_event`]).
///
/// A reserved ID that already appears in this writer's segments is returned as
/// the original event when its content matches the draft, so a retried operation
/// cannot append a duplicate. A reserved ID used by different content freezes the
/// writer instead of guessing.
pub fn append_event_reserved(
    vault: &Vault,
    local: &LocalPaths,
    key: &WriterKey,
    draft: EventDraft,
    reserved: Option<EventId>,
    clock: &dyn crate::vault::Clock,
    ids: &mut dyn crate::vault::IdSource,
) -> Result<AppendResult> {
    let mut results = append_events_reserved(
        vault,
        local,
        key,
        draft,
        reserved,
        &|_| Vec::new(),
        clock,
        ids,
    )?;
    Ok(results.remove(0))
}

/// Append the events of one command in one write call: `draft`, then the
/// drafts `following` builds from its event ID, in that order and with
/// consecutive sequence numbers. One event is returned per draft.
///
/// Every line of a write is validated before that write. The lines are one
/// pending record and one buffer, so the segment grows once; a write that stops
/// in the middle leaves whole lines and at most one unfinished line, which the
/// next append of this writer completes from the pending record. One write is
/// never split over two segments. Lines that together exceed
/// [`MAX_LINE_BYTES`] take more than one write, each as large as fits, so a
/// pending record keeps the bound it has for one line.
///
/// A draft whose reserved ID is already in this writer's segments is returned
/// as the original event and not written again (see [`append_event_reserved`]);
/// the other drafts are still appended. When the pending record just completed
/// holds exactly these drafts, its events are returned and nothing is written.
#[allow(clippy::too_many_arguments)]
pub fn append_events_reserved(
    vault: &Vault,
    local: &LocalPaths,
    key: &WriterKey,
    draft: EventDraft,
    reserved: Option<EventId>,
    following: Following<'_>,
    clock: &dyn crate::vault::Clock,
    ids: &mut dyn crate::vault::IdSource,
) -> Result<Vec<AppendResult>> {
    let layout = local.ensure_layout(vault.root(), vault.id())?;
    let lock_path = layout.locks.join(lock_file_name(vault, key));
    let _lock = acquire_lock(&lock_path)?;
    let journal_path = layout.pending.join(journal_file_name(vault, key));
    let payload_path = layout.pending.join(payload_file_name(vault, key));
    let mut journal = load_journal(&journal_path, key)?;

    if let Some(detail) = journal.forked.clone() {
        return Err(fork_error(&detail));
    }
    if journal.sealed == Some(SealReason::SessionEnded) {
        return Err(sealed_error());
    }

    if let Some(recovered) = recover_locked(
        vault,
        &layout,
        key,
        &journal_path,
        &payload_path,
        &mut journal,
    )? && batch_matches(&recovered, key, &draft, following)?
    {
        return Ok(recovered.results);
    }
    let mut queue = VecDeque::from([(draft, reserved)]);
    let mut results = Vec::new();
    while !queue.is_empty() {
        // Each write takes at least one draft, so only the first one has no
        // result yet; the followers are built once, from its event ID.
        let following = results.is_empty().then_some(following);
        results.extend(append_pending(
            vault,
            &layout,
            key,
            &mut journal,
            &journal_path,
            &payload_path,
            &mut queue,
            following,
            clock,
            ids,
        )?);
    }
    Ok(results)
}

/// One write call under the writer lock: the drafts at the head of `queue`
/// whose lines together fit [`MAX_LINE_BYTES`] (always at least one), so a
/// pending record never exceeds the length of one line. A draft that does not
/// fit stays in `queue` with its ID for the next write.
#[allow(clippy::too_many_arguments)]
fn append_pending(
    vault: &Vault,
    layout: &crate::runtime::LocalLayout,
    key: &WriterKey,
    journal: &mut WriterJournal,
    journal_path: &Path,
    payload_path: &Path,
    queue: &mut VecDeque<(EventDraft, Option<EventId>)>,
    following: Option<Following<'_>>,
    clock: &dyn crate::vault::Clock,
    ids: &mut dyn crate::vault::IdSource,
) -> Result<Vec<AppendResult>> {
    let mut scan = scan_writer(vault, key)?;
    let mut next_seq = scan.max_seq.saturating_add(1).max(journal.next_seq);

    let mut slots: Vec<Slot> = Vec::new();
    let mut batch_bytes = 0_usize;
    while let Some((draft, reserved)) = queue.pop_front() {
        let slot = if let Some(reserved) = &reserved
            && scan.ids.contains_key(reserved)
        {
            Slot::Existing(existing_draft_event(vault, key, reserved, &draft)?)
        } else {
            if next_seq > MAX_SAFE_SEQ {
                return Err(AgendarError::validation_at(
                    "/seq",
                    "seq_exhausted",
                    "The writer stream exhausted the JavaScript-safe sequence range.",
                ));
            }
            let event_id = match reserved {
                Some(id) => id,
                None => next_unused_id(ids, &scan.ids)?,
            };
            let event = Event {
                v: 1,
                id: event_id,
                seq: next_seq,
                writer: key.writer.clone(),
                ts: clock.now(),
                project: key.project.clone(),
                refs: draft.refs,
                payload: draft.payload,
            };
            let line = canonical_event_line(&event)?;
            if line.len() > MAX_LINE_BYTES {
                return Err(AgendarError::validation_at(
                    "/",
                    "event_line_too_long",
                    format!(
                        "A complete event line must be at most {MAX_LINE_BYTES} bytes including LF; got {}.",
                        line.len()
                    ),
                ));
            }
            if batch_bytes > 0 && batch_bytes.saturating_add(line.len()) > MAX_LINE_BYTES {
                queue.push_front((
                    EventDraft {
                        refs: event.refs,
                        payload: event.payload,
                    },
                    Some(event.id),
                ));
                break;
            }
            batch_bytes = batch_bytes.saturating_add(line.len());
            // A later draft of this batch never takes this ID again.
            scan.ids
                .insert(event.id.clone(), Sha256::from_bytes(line.as_bytes()));
            next_seq = next_seq.saturating_add(1);
            Slot::New { event, line }
        };
        if slots.is_empty()
            && let Some(following) = following
        {
            let first = match &slot {
                Slot::Existing(existing) => &existing.event.id,
                Slot::New { event, .. } => &event.id,
            };
            queue.extend(following(first));
        }
        slots.push(slot);
    }
    // Whole lines only, in draft order: the one buffer the segment receives.
    let line: String = slots
        .iter()
        .filter_map(|slot| match slot {
            Slot::New { line, .. } => Some(line.as_str()),
            Slot::Existing(_) => None,
        })
        .collect();
    if line.is_empty() {
        return Ok(slots
            .into_iter()
            .filter_map(|slot| match slot {
                Slot::Existing(existing) => Some(existing),
                Slot::New { .. } => None,
            })
            .collect());
    }

    let (segment, abandoned) = choose_segment(&scan, journal, line.len() as u64);
    let relative_path = writer_relative_path(&key.project, &key.writer, segment)?;
    let source = SourcePath::from_relative_path(&relative_path)?;
    // Full model validation before any journal or Vault write.
    for slot in &slots {
        if let Slot::New { line, .. } = slot {
            parse_event(line.as_bytes(), &source)?;
        }
    }

    let host_dir = ensure_host_directory(vault.root(), &key.project, key.writer.host())?;
    let target = host_dir.join(writer_file_name(&key.writer, segment)?);
    if let Ok(metadata) = fs::symlink_metadata(&target)
        && metadata.file_type().is_symlink()
    {
        return Err(AgendarError::validation_at(
            "/vault-path",
            "vault_symlink",
            "Writer segments must not be symbolic links.",
        ));
    }
    let target_size = match fs::metadata(&target) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };
    let max_segment = scan.max_segment.unwrap_or(0);
    let new_segment = segment > max_segment;
    if new_segment {
        if target_size != 0 {
            return Err(fork_error("new writer segment already exists"));
        }
    } else if segment != max_segment || target_size != scan.last_size {
        return Err(fork_error("writer segment changed under the lock"));
    }
    let offset = target_size;

    let digest = Sha256::from_bytes(line.as_bytes());
    write_bytes_atomic(payload_path, line.as_bytes())?;
    journal.pending = Some(PendingAppend {
        path: relative_path.clone(),
        offset,
        bytes: line.len() as u64,
        digest,
    });
    if let Some(abandoned) = abandoned
        && !journal
            .abandoned
            .iter()
            .any(|existing| existing.path == abandoned.path && existing.offset == abandoned.offset)
    {
        journal.abandoned.push(abandoned);
    }
    write_json_atomic(journal_path, journal)?;

    let appended = (|| -> std::io::Result<()> {
        let mut file = if new_segment {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?
        } else {
            OpenOptions::new().append(true).open(&target)?
        };
        count_segment_write();
        file.write_all(line.as_bytes())?;
        file.sync_data()?;
        Ok(())
    })();
    if let Err(error) = appended {
        return Err(vault_io_error("append the writer segment", error));
    }
    if new_segment {
        sync_dir(&host_dir)?;
    }

    let written = read_range(&target, offset, line.len())?;
    if written.as_slice() != line.as_bytes() {
        return Err(fork_error(
            "writer suffix does not match the pending append",
        ));
    }

    journal.pending = None;
    journal.next_seq = next_seq;
    write_json_atomic(journal_path, journal)?;
    remove_if_present(payload_path)?;
    sync_dir(&layout.pending)?;

    let mut byte_end = offset;
    Ok(slots
        .into_iter()
        .map(|slot| match slot {
            Slot::Existing(existing) => existing,
            Slot::New { event, line } => {
                byte_end = byte_end.saturating_add(line.len() as u64);
                AppendResult {
                    event,
                    relative_path: relative_path.clone(),
                    byte_end,
                }
            }
        })
        .collect())
}

/// Complete any durable pending append for this writer without consuming a new
/// draft, and return its last event. Used by service startup and recovery tests.
pub fn recover_writer(
    vault: &Vault,
    local: &LocalPaths,
    key: &WriterKey,
) -> Result<Option<AppendResult>> {
    let layout = local.ensure_layout(vault.root(), vault.id())?;
    let _lock = acquire_lock(&layout.locks.join(lock_file_name(vault, key)))?;
    let journal_path = layout.pending.join(journal_file_name(vault, key));
    let payload_path = layout.pending.join(payload_file_name(vault, key));
    let mut journal = load_journal(&journal_path, key)?;
    if let Some(detail) = journal.forked.clone() {
        return Err(fork_error(&detail));
    }
    Ok(recover_locked(
        vault,
        &layout,
        key,
        &journal_path,
        &payload_path,
        &mut journal,
    )?
    .and_then(|recovered| recovered.results.into_iter().next_back()))
}

/// Seal a writer. `SessionEnded` is permanent and refuses later appends;
/// `Size` closes the current segment so the next append starts a new one.
pub fn seal_writer(
    vault: &Vault,
    local: &LocalPaths,
    key: &WriterKey,
    reason: SealReason,
) -> Result<()> {
    if reason == SealReason::SessionEnded && !key.writer.is_agent() {
        return Err(AgendarError::validation_at(
            "/reason",
            "seal_reason",
            "Only an agent session writer may be sealed at session end.",
        ));
    }
    let layout = local.ensure_layout(vault.root(), vault.id())?;
    let _lock = acquire_lock(&layout.locks.join(lock_file_name(vault, key)))?;
    let journal_path = layout.pending.join(journal_file_name(vault, key));
    let payload_path = layout.pending.join(payload_file_name(vault, key));
    let mut journal = load_journal(&journal_path, key)?;

    match reason {
        SealReason::SessionEnded => journal.sealed = Some(SealReason::SessionEnded),
        SealReason::Size => {
            let scan = scan_writer(vault, key)?;
            let current = scan.max_segment.unwrap_or(0);
            journal.sealed_segment = Some(journal.sealed_segment.unwrap_or(0).max(current));
        }
    }
    write_json_atomic(&journal_path, &journal)?;

    if let Some(detail) = journal.forked.clone() {
        return Err(fork_error(&detail));
    }
    recover_locked(
        vault,
        &layout,
        key,
        &journal_path,
        &payload_path,
        &mut journal,
    )?;
    Ok(())
}

/// Path of the writer lock file. Callers must have created the local layout.
pub fn writer_lock_path(local: &LocalPaths, vault: &Vault, key: &WriterKey) -> PathBuf {
    local.locks_dir(vault.id()).join(lock_file_name(vault, key))
}

/// Path of the writer journal file. Callers must have created the local layout.
pub fn writer_journal_path(local: &LocalPaths, vault: &Vault, key: &WriterKey) -> PathBuf {
    local
        .pending_dir(vault.id())
        .join(journal_file_name(vault, key))
}

fn lock_file_name(vault: &Vault, key: &WriterKey) -> String {
    format!("writer-{}.lock", key_hash("writer-lock", vault, key))
}

fn journal_file_name(vault: &Vault, key: &WriterKey) -> String {
    format!("writer-{}.json", key_hash("writer-journal", vault, key))
}

fn payload_file_name(vault: &Vault, key: &WriterKey) -> String {
    format!("writer-{}.pending", key_hash("writer-journal", vault, key))
}

fn key_hash(domain: &str, vault: &Vault, key: &WriterKey) -> Sha256 {
    let value = serde_json::json!([
        domain,
        vault.id().as_str(),
        key.project.as_str(),
        key.writer.to_string()
    ]);
    Sha256::from_bytes(canonical_json(&value).as_bytes())
}

fn load_journal(path: &Path, key: &WriterKey) -> Result<WriterJournal> {
    let Some(value) = read_json_value(path)? else {
        return Ok(WriterJournal::new(key));
    };
    let version = value
        .get("v")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            AgendarError::validation_at(
                "/local-state",
                "writer_journal_version",
                "The writer journal has no version field.",
            )
        })?;
    if version != u64::from(JOURNAL_VERSION) {
        return Err(AgendarError::new(
            ErrorCode::Unsupported,
            format!("Writer journal version {version} is not supported."),
        )
        .with_detail("writer_journal_version")
        .with_hint("Use the Agendar version that created this local journal."));
    }
    let journal: WriterJournal = serde_json::from_value(value).map_err(|error| {
        AgendarError::validation_at(
            "/local-state",
            "writer_journal_malformed",
            format!("The writer journal has an unexpected shape: {error}."),
        )
    })?;
    if journal.project != key.project || journal.writer != key.writer {
        return Err(AgendarError::new(
            ErrorCode::Io,
            "Cannot use local writer state: it belongs to another writer.",
        )
        .with_detail("writer_journal_mismatch"));
    }
    Ok(journal)
}

fn recover_locked(
    vault: &Vault,
    layout: &crate::runtime::LocalLayout,
    key: &WriterKey,
    journal_path: &Path,
    payload_path: &Path,
    journal: &mut WriterJournal,
) -> Result<Option<Recovered>> {
    let Some(pending) = journal.pending.clone() else {
        // A payload without a journal record is an unreferenced temporary; it
        // is never authoritative.
        if fs::symlink_metadata(payload_path).is_ok() {
            remove_if_present(payload_path)?;
        }
        return Ok(None);
    };
    if pending.bytes == 0 || pending.bytes > MAX_LINE_BYTES as u64 {
        freeze(
            journal,
            journal_path,
            "pending append has an invalid length",
        )?;
        return Err(fork_error("pending append has an invalid length"));
    }

    let source = match SourcePath::from_relative_path(&pending.path) {
        Ok(source) if source.project == key.project && source.writer == key.writer => source,
        _ => {
            freeze(
                journal,
                journal_path,
                "pending append path is not this writer",
            )?;
            return Err(fork_error("pending append path is not this writer"));
        }
    };

    let target = contained_path(vault.root(), &pending.path)?;
    let size = match fs::metadata(&target) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };
    if size < pending.offset {
        freeze(
            journal,
            journal_path,
            "writer segment is shorter than its journal",
        )?;
        return Err(fork_error("writer segment is shorter than its journal"));
    }
    let available = size - pending.offset;
    if available > pending.bytes {
        // Bytes past the journalled suffix are not this append; a matching line
        // followed by anything else is a conflicting history, never a success.
        freeze(
            journal,
            journal_path,
            "writer segment is longer than its pending append",
        )?;
        return Err(fork_error(
            "writer segment is longer than its pending append",
        ));
    }

    // A durable pending record is committed intent. Missing, unreadable or
    // mismatching bytes must never be silently treated as an abandoned append:
    // a transient read error preserves the reservation for a later retry and a
    // corrupt reservation freezes the writer.
    let line = match read_pending_line(payload_path, &pending)? {
        PendingBytes::Verified(line) => line,
        PendingBytes::Unverifiable(detail) => {
            freeze(journal, journal_path, detail)?;
            return Err(fork_error(detail));
        }
    };
    let host_relative = pending
        .path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or_default();
    if host_relative.is_empty() {
        freeze(
            journal,
            journal_path,
            "pending append path has no directory",
        )?;
        return Err(fork_error("pending append path has no directory"));
    }
    ensure_directory(vault.root(), host_relative)?;

    if available < pending.bytes {
        if available > 0 {
            let prefix = read_range(&target, pending.offset, available as usize)?;
            if prefix != line[..available as usize] {
                freeze(
                    journal,
                    journal_path,
                    "writer suffix conflicts with the pending append",
                )?;
                return Err(fork_error(
                    "writer suffix conflicts with the pending append",
                ));
            }
        }
        let exists = fs::symlink_metadata(&target).is_ok();
        let mut file = if exists {
            OpenOptions::new().append(true).open(&target)?
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?
        };
        count_segment_write();
        file.write_all(&line[available as usize..])?;
        file.sync_data()?;
    }

    let full = read_range(&target, pending.offset, pending.bytes as usize)?;
    if full != line {
        freeze(
            journal,
            journal_path,
            "writer suffix does not match the pending append",
        )?;
        return Err(fork_error(
            "writer suffix does not match the pending append",
        ));
    }
    // Readable bytes prove the write reached the page cache, not the medium. The
    // dead append may have stopped between `write_all` and `sync_data`, and a
    // segment created by that process may have a readable prefix whose directory
    // entry was never published. The verified segment is therefore flushed
    // together with its parent entry before the journal and payload are dropped,
    // regardless of which process created it or whether this recovery appended
    // the missing suffix. A flush failure keeps the recovery material.
    sync_segment(&target)?;

    let mut results = Vec::new();
    let mut lines = Vec::new();
    let mut next_seq = journal.next_seq;
    let mut byte_end = pending.offset;
    for one in line.split_inclusive(|byte| *byte == b'\n') {
        let event = parse_event(one, &source)?;
        next_seq = next_seq.max(event.seq.saturating_add(1));
        byte_end = byte_end.saturating_add(one.len() as u64);
        results.push(AppendResult {
            event,
            relative_path: pending.path.clone(),
            byte_end,
        });
        lines.push(one.to_vec());
    }
    journal.pending = None;
    journal.next_seq = next_seq.max(1);
    write_json_atomic(journal_path, journal)?;
    remove_if_present(payload_path)?;
    sync_dir(&layout.pending)?;

    Ok(Some(Recovered { results, lines }))
}

fn freeze(journal: &mut WriterJournal, journal_path: &Path, detail: &str) -> Result<()> {
    journal.forked = Some(detail.to_owned());
    write_json_atomic(journal_path, journal)
}

/// The exact pending line bytes, or a description of why the committed
/// reservation cannot be verified. A `Verified` line is safe to complete; an
/// `Unverifiable` reservation freezes the writer.
enum PendingBytes {
    Verified(Vec<u8>),
    Unverifiable(&'static str),
}

fn read_pending_line(payload_path: &Path, pending: &PendingAppend) -> Result<PendingBytes> {
    let metadata = match fs::symlink_metadata(payload_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PendingBytes::Unverifiable(
                "pending append bytes are missing",
            ));
        }
        Err(error) => return Err(unreadable_pending_bytes(error)),
    };
    if metadata.file_type().is_symlink() {
        return Ok(PendingBytes::Unverifiable(
            "pending append bytes are a symbolic link",
        ));
    }
    if !metadata.is_file() || metadata.len() != pending.bytes {
        return Ok(PendingBytes::Unverifiable(
            "pending append is not a matching regular file",
        ));
    }
    let bytes = match fs::read(payload_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PendingBytes::Unverifiable(
                "pending append bytes are missing",
            ));
        }
        // A temporarily unreadable payload is a transient condition, not proof
        // that the reservation was abandoned.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Interrupted
            ) =>
        {
            return Err(unreadable_pending_bytes(error));
        }
        Err(_) => {
            return Ok(PendingBytes::Unverifiable(
                "pending append bytes cannot be read",
            ));
        }
    };
    if Sha256::from_bytes(&bytes) != pending.digest {
        return Ok(PendingBytes::Unverifiable(
            "pending append bytes do not match the journal digest",
        ));
    }
    Ok(PendingBytes::Verified(bytes))
}

fn unreadable_pending_bytes(error: std::io::Error) -> AgendarError {
    AgendarError::new(
        ErrorCode::Io,
        format!(
            "Cannot read the pending append bytes: {}; the reservation is preserved.",
            safe_os_reason(&error)
        ),
    )
    .with_detail("pending_bytes_unreadable")
    .with_hint("Check local permissions and retry; never delete the pending journal or bytes.")
    .with_retryable(true)
}

/// Flush an existing segment and its directory entry. A segment that cannot be
/// opened for flushing fails recovery without clearing its journal.
fn sync_segment(path: &Path) -> Result<()> {
    let file = OpenOptions::new().append(true).open(path)?;
    file.sync_data()?;
    drop(file);
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

/// Whether the completed pending append holds exactly these drafts, in order.
fn batch_matches(
    recovered: &Recovered,
    key: &WriterKey,
    draft: &EventDraft,
    following: Following<'_>,
) -> Result<bool> {
    let (Some(first), Some(first_line)) = (recovered.results.first(), recovered.lines.first())
    else {
        return Ok(false);
    };
    if !draft_matches(&first.event, first_line, key, draft)? {
        return Ok(false);
    }
    let rest = following(&first.event.id);
    if rest.len().saturating_add(1) != recovered.results.len() {
        return Ok(false);
    }
    for ((draft, _), (result, line)) in rest
        .iter()
        .zip(recovered.results.iter().zip(&recovered.lines).skip(1))
    {
        if !draft_matches(&result.event, line, key, draft)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn draft_matches(event: &Event, line: &[u8], key: &WriterKey, draft: &EventDraft) -> Result<bool> {
    let candidate = Event {
        v: 1,
        id: event.id.clone(),
        seq: event.seq,
        writer: key.writer.clone(),
        ts: event.ts,
        project: key.project.clone(),
        refs: draft.refs.clone(),
        payload: draft.payload.clone(),
    };
    Ok(canonical_event_line(&candidate)?.as_bytes() == line)
}

/// Return an already durable event for a reserved ID without appending it again.
///
/// The reserved line is found by decoding this writer's own complete lines only;
/// the candidate line is rebuilt from the observed envelope and the supplied draft
/// and compared byte for byte. A reserved ID whose content differs is a fork, not
/// a retry, so the writer is frozen rather than silently accepting the draft.
fn existing_draft_event(
    vault: &Vault,
    key: &WriterKey,
    reserved: &EventId,
    draft: &EventDraft,
) -> Result<AppendResult> {
    let scan = scan_writer(vault, key)?;
    let mut segments = scan.segments.clone();
    segments.sort_by_key(|info| info.segment);
    for info in &segments {
        let source = SourcePath::from_relative_path(&info.relative)?;
        let path = contained_path(vault.root(), &info.relative)?;
        let mut offset = 0_u64;
        loop {
            let batch = read_complete_lines(&path, offset, crate::vault::MAX_BATCH_BYTES)?;
            for line in &batch.lines {
                if line.oversized {
                    continue;
                }
                let Ok(event) = parse_event(&line.bytes, &source) else {
                    continue;
                };
                if &event.id != reserved {
                    continue;
                }
                let candidate = Event {
                    v: 1,
                    id: event.id.clone(),
                    seq: event.seq,
                    writer: key.writer.clone(),
                    ts: event.ts,
                    project: key.project.clone(),
                    refs: draft.refs.clone(),
                    payload: draft.payload.clone(),
                };
                if canonical_event_line(&candidate)?.into_bytes() != line.bytes {
                    return Err(fork_error(
                        "a reserved event ID was already used with different content",
                    ));
                }
                return Ok(AppendResult {
                    event: candidate,
                    relative_path: info.relative.clone(),
                    byte_end: line.end,
                });
            }
            if batch.trailing_partial || batch.next_offset <= offset {
                break;
            }
            offset = batch.next_offset;
            if offset >= batch.observed_size {
                break;
            }
        }
    }
    Err(fork_error(
        "a reserved event ID is present but cannot be decoded",
    ))
}

fn canonical_event_line(event: &Event) -> Result<String> {
    let value = serde_json::to_value(event).map_err(|error| {
        AgendarError::new(
            ErrorCode::Io,
            format!("Cannot serialize the event: {error}."),
        )
    })?;
    let mut line = canonical_json(&value);
    line.push('\n');
    Ok(line)
}

fn choose_segment(
    scan: &Scan,
    journal: &WriterJournal,
    line_len: u64,
) -> (u32, Option<AbandonedTail>) {
    let max_segment = scan.max_segment.unwrap_or(0);
    let sealed_segment = journal.sealed_segment.unwrap_or(0);
    if max_segment == 0 {
        return ((1_u32).max(sealed_segment.saturating_add(1)), None);
    }
    if scan.partial_tail {
        let tail = scan
            .segments
            .iter()
            .find(|info| info.segment == max_segment);
        return (
            max_segment.saturating_add(1),
            tail.map(|info| AbandonedTail {
                path: info.relative.clone(),
                offset: scan.partial_offset.unwrap_or(info.size),
            }),
        );
    }
    if sealed_segment >= max_segment {
        return (max_segment.saturating_add(1), None);
    }
    if segment_would_seal(scan.last_size, line_len) {
        return (max_segment.saturating_add(1), None);
    }
    (max_segment, None)
}

fn next_unused_id(
    ids: &mut dyn crate::vault::IdSource,
    existing: &BTreeMap<EventId, Sha256>,
) -> Result<EventId> {
    for _ in 0..ID_ATTEMPTS {
        let id = ids.next_event_id();
        if !existing.contains_key(&id) {
            return Ok(id);
        }
    }
    Err(
        AgendarError::new(ErrorCode::Io, "Cannot allocate an unused event ID.")
            .with_detail("id_exhausted"),
    )
}

fn scan_writer(vault: &Vault, key: &WriterKey) -> Result<Scan> {
    let mut scan = Scan::default();
    let host_relative = format!("projects/{}/{}", key.project.as_str(), key.writer.host());
    let host_dir = match fs::metadata(vault.root().join(&host_relative)) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(scan),
        Err(error) => return Err(error.into()),
    };
    if !host_dir.is_dir() {
        return Err(AgendarError::validation_at(
            "/vault-path",
            "vault_path_not_directory",
            "The writer host path is not a directory.",
        ));
    }
    let host_dir = contained_path(vault.root(), &host_relative)?;

    for entry in fs::read_dir(&host_dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(AgendarError::validation_at(
                "/vault-path",
                "vault_symlink",
                "Writer segments must not be symbolic links.",
            ));
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let relative = format!("{host_relative}/{name}");
        let Ok(source) = SourcePath::from_relative_path(&relative) else {
            continue;
        };
        if source.project != key.project || source.writer != key.writer {
            continue;
        }
        let size = entry.metadata()?.len();
        scan.segments.push(SegmentInfo {
            segment: source.segment,
            relative,
            size,
        });
    }
    scan.segments.sort_by_key(|info| info.segment);
    for pair in scan.segments.windows(2) {
        if pair[0].segment == pair[1].segment {
            return Err(fork_error("duplicate writer segment number"));
        }
    }
    scan.max_segment = scan.segments.last().map(|info| info.segment);
    scan.last_size = scan.segments.last().map_or(0, |info| info.size);

    let mut seqs: BTreeMap<u64, (EventId, Sha256)> = BTreeMap::new();
    for info in &scan.segments {
        let source = SourcePath::from_relative_path(&info.relative)?;
        let path = contained_path(vault.root(), &info.relative)?;
        let mut offset = 0_u64;
        loop {
            let batch = read_complete_lines(&path, offset, crate::vault::MAX_BATCH_BYTES)?;
            if batch.lines.is_empty() && !batch.trailing_partial {
                break;
            }
            for line in &batch.lines {
                if line.oversized {
                    if let Some(seq) = scan_seq_prefix(&line.bytes) {
                        scan.max_seq = scan.max_seq.max(seq);
                    }
                    continue;
                }
                match parse_event(&line.bytes, &source) {
                    Ok(event) => {
                        let digest = Sha256::from_bytes(&line.bytes);
                        if let Some((previous_id, previous_digest)) =
                            seqs.insert(event.seq, (event.id.clone(), digest.clone()))
                            && (previous_id != event.id || previous_digest != digest)
                        {
                            return Err(fork_error(
                                "the same sequence appears with different events",
                            ));
                        }
                        if let Some(previous) = scan.ids.insert(event.id.clone(), digest.clone())
                            && previous != digest
                        {
                            return Err(fork_error(
                                "the same event ID appears with different payloads",
                            ));
                        }
                        scan.max_seq = scan.max_seq.max(event.seq);
                    }
                    Err(_) => {
                        if let Some(seq) = scan_seq_prefix(&line.bytes) {
                            scan.max_seq = scan.max_seq.max(seq);
                        }
                    }
                }
            }
            if batch.trailing_partial {
                if info.segment == scan.max_segment.unwrap_or(0) {
                    scan.partial_offset = Some(batch.next_offset);
                }
                break;
            }
            if batch.next_offset <= offset {
                break;
            }
            offset = batch.next_offset;
            if offset >= batch.observed_size {
                break;
            }
        }
    }

    if let Some(last) = scan.segments.last() {
        let path = contained_path(vault.root(), &last.relative)?;
        if last_byte_is_lf(&path)? == Some(false) {
            scan.partial_tail = true;
        }
    }
    Ok(scan)
}

/// Minimal `"seq"` extraction for a malformed or oversized line so sequence
/// allocation stays monotonic even when one line is damaged.
fn scan_seq_prefix(bytes: &[u8]) -> Option<u64> {
    let needle = b"\"seq\"";
    let mut index = 0;
    while let Some(found) = find_subslice(&bytes[index..], needle) {
        let mut cursor = index + found + needle.len();
        while bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            cursor += 1;
        }
        if bytes.get(cursor) == Some(&b':') {
            cursor += 1;
            while bytes
                .get(cursor)
                .is_some_and(|byte| byte.is_ascii_whitespace())
            {
                cursor += 1;
            }
            let start = cursor;
            while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                cursor += 1;
            }
            if cursor > start
                && let Ok(text) = std::str::from_utf8(&bytes[start..cursor])
                && let Ok(value) = text.parse::<u64>()
            {
                return Some(value);
            }
        }
        index += found + needle.len();
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn read_range(path: &Path, offset: u64, length: usize) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0_u8; length];
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn fork_error(detail: &str) -> AgendarError {
    AgendarError::new(
        ErrorCode::WriterFork,
        "Conflicting writer history; writes are frozen.",
    )
    .with_detail(detail)
    .with_hint("Inspect the writer stream; do not edit or delete Vault event lines.")
}

fn sealed_error() -> AgendarError {
    AgendarError::new(
        ErrorCode::WriterFork,
        "Writer is sealed; writes are frozen.",
    )
    .with_detail("writer_sealed")
    .with_hint("Allocate a new session ID for new work.")
}
