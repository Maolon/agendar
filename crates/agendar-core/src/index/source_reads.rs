//! Bounded reads of Vault source files.
//!
//! A sync app (any product: iCloud Drive, Dropbox, OneDrive, Google Drive,
//! Syncthing, an rclone mount or one we do not know) may keep a Vault file as
//! an online-only placeholder. Reading such a file blocks in the OS until the
//! download finishes, or fails offline, and a blocked read cannot be
//! cancelled. So every read of source bytes runs on its own thread and the
//! caller waits for it only up to a deadline; a read that is not done in time
//! leaves the source "waiting" and the caller carries on with its last stored
//! state.
//!
//! At most one read per source path is outstanding in this tracker. A later
//! request for the same path reuses it instead of starting another thread, and
//! its result, once finished, is picked up by the next request while the file
//! is unchanged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::index::ingest::{DecodedLine, read_consumed};
use crate::model::Sha256;

/// How long a caller waits for one source read.
///
/// Writer files seal at 1 MiB, so a local read takes a few milliseconds even
/// on a slow disk or with an antivirus scan on open; 250 ms is far above that.
/// It is also small enough for the app, which refreshes while holding its
/// command state: after the first read that misses its deadline, every other
/// source still to read is started at once and shares one more deadline, so a
/// refresh waits at most about twice this (0.5 s) for source bytes, and a
/// source already known to be slow is not waited for again.
pub const READ_BUDGET: Duration = Duration::from_millis(250);

/// macOS and iOS `st_flags` bit for a file whose content is not on this
/// device (`SF_DATALESS` in `<sys/stat.h>`). iCloud Drive and every File
/// Provider client (Dropbox, OneDrive, Google Drive, Box, ...) set it.
pub const SF_DATALESS: u32 = 0x4000_0000;
/// Windows attribute: the file's data is not immediately available.
pub const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
/// Windows attribute: opening the file fetches it from remote storage.
pub const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
/// Windows attribute: reading the file's data fetches it from remote storage
/// (cloud files placeholders: OneDrive, Dropbox, Google Drive, iCloud, ...).
pub const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// The platform metadata bits that say whether a file's content is local.
/// Linux has no such flag; both fields are zero there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaceholderBits {
    /// macOS and iOS `st_flags`.
    pub st_flags: u32,
    /// Windows file attributes.
    pub file_attributes: u32,
}

/// Whether a file's content still has to be downloaded by a sync app.
pub fn content_not_local(bits: PlaceholderBits) -> bool {
    bits.st_flags & SF_DATALESS != 0
        || bits.file_attributes
            & (FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_RECALL_ON_OPEN
                | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
            != 0
}

/// The placeholder bits of one file's metadata (no content is read).
pub fn placeholder_bits(metadata: &std::fs::Metadata) -> PlaceholderBits {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt as _;
        PlaceholderBits {
            st_flags: metadata.st_flags(),
            file_attributes: 0,
        }
    }
    #[cfg(target_os = "ios")]
    {
        use std::os::ios::fs::MetadataExt as _;
        PlaceholderBits {
            st_flags: metadata.st_flags(),
            file_attributes: 0,
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        PlaceholderBits {
            st_flags: 0,
            file_attributes: metadata.file_attributes(),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios", windows)))]
    {
        let _ = metadata;
        PlaceholderBits::default()
    }
}

/// What one read asks for: complete lines from `start_offset`, whose prefix
/// must still hash to `expected_prefix` (the stored cursor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    pub start_offset: u64,
    pub expected_prefix: Option<Sha256>,
}

/// One complete read of a source up to its last complete line.
#[derive(Debug, Clone)]
pub struct SourceRead {
    /// Where `lines` begin: the requested offset, or 0 after a rewrite.
    pub start_offset: u64,
    /// The stored prefix no longer matched (or the file shrank), so the file
    /// was read again from the beginning.
    pub rewritten: bool,
    pub lines: Vec<DecodedLine>,
    /// Exclusive end of the complete-line prefix.
    pub next_offset: u64,
    /// SHA-256 of `[0, next_offset)`.
    pub prefix_sha256: Sha256,
    pub observed_size: u64,
    pub trailing_partial: bool,
}

/// Read one source in full from the requested offset (the production reader).
pub fn read_source(path: &Path, request: &ReadRequest) -> Result<SourceRead> {
    if request.start_offset > 0 {
        match read_from(path, request.start_offset) {
            Ok((read, prefix_at_start)) => {
                let intact = match &request.expected_prefix {
                    Some(expected) => prefix_at_start.as_ref() == Some(expected),
                    None => true,
                };
                if intact {
                    return Ok(read);
                }
            }
            Err(error) if error.detail() == Some("source_shrunk") => {}
            Err(error) => return Err(error),
        }
        let (mut read, _) = read_from(path, 0)?;
        read.rewritten = true;
        return Ok(read);
    }
    read_from(path, 0).map(|(read, _)| read)
}

fn read_from(path: &Path, start_offset: u64) -> Result<(SourceRead, Option<Sha256>)> {
    let mut offset = start_offset;
    let mut lines = Vec::new();
    let mut first_prefix: Option<Option<Sha256>> = None;
    loop {
        let snapshot = read_consumed(path, offset)?;
        if first_prefix.is_none() {
            first_prefix = Some(snapshot.prefix_at_start.clone());
        }
        lines.extend(snapshot.lines);
        let previous = offset;
        offset = snapshot.next_offset;
        if snapshot.trailing_partial || offset >= snapshot.observed_size || offset == previous {
            return Ok((
                SourceRead {
                    start_offset,
                    rewritten: false,
                    lines,
                    next_offset: offset,
                    prefix_sha256: snapshot.prefix_sha256,
                    observed_size: snapshot.observed_size,
                    trailing_partial: snapshot.trailing_partial,
                },
                first_prefix.flatten(),
            ));
        }
    }
}

/// A source reader: the production [`read_source`], or a test stand-in.
pub type SourceReader = dyn Fn(&Path, &ReadRequest) -> Result<SourceRead> + Send + Sync;

/// What a bounded read produced.
#[derive(Debug, Clone)]
pub enum ReadOutcome {
    Ready(Box<SourceRead>),
    /// The content did not arrive in time (or the file is flagged as not
    /// local); a read is outstanding in the background.
    Waiting,
    Failed(AgendarError),
}

/// What a caller knows about the file before reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// Size and modification time; a finished background read is used only
    /// while they are unchanged.
    pub signature: Option<(u64, Option<SystemTime>)>,
    /// The platform says the content is not local: do not wait for it.
    pub placeholder: bool,
}

impl Probe {
    pub fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            signature: Some((metadata.len(), metadata.modified().ok())),
            placeholder: content_not_local(placeholder_bits(metadata)),
        }
    }
}

struct Slot {
    request: ReadRequest,
    signature: Option<(u64, Option<SystemTime>)>,
    state: Mutex<SlotState>,
    done: Condvar,
}

#[derive(Default)]
struct SlotState {
    result: Option<Result<SourceRead>>,
    /// A caller already gave up waiting: later callers do not wait again.
    overdue: bool,
}

enum Begin {
    Done(Result<SourceRead>),
    Waiting,
    Pending(Arc<Slot>),
}

/// Bounded source reads with at most one outstanding read per path.
pub struct SourceReads {
    reader: Arc<SourceReader>,
    budget: Duration,
    slots: Mutex<HashMap<PathBuf, Arc<Slot>>>,
}

impl std::fmt::Debug for SourceReads {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceReads")
            .field("budget", &self.budget)
            .field("in_flight", &self.in_flight())
            .finish()
    }
}

impl SourceReads {
    /// A tracker with its own reader and per-source wait (tests use a reader
    /// that blocks).
    pub fn new(budget: Duration, reader: Arc<SourceReader>) -> Self {
        Self {
            reader,
            budget,
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// The process-wide tracker every Service shares, so one process never
    /// starts a second read of a path whose first read is still blocked.
    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<SourceReads>> = OnceLock::new();
        Arc::clone(
            SHARED.get_or_init(|| Arc::new(SourceReads::new(READ_BUDGET, Arc::new(read_source)))),
        )
    }

    pub fn budget(&self) -> Duration {
        self.budget
    }

    /// Reads still running in the background.
    pub fn in_flight(&self) -> usize {
        self.slots()
            .values()
            .filter(|slot| slot_state(slot).result.is_none())
            .count()
    }

    /// Read several sources. Each waits up to the budget; after the first
    /// miss, every remaining read starts at once and shares one deadline.
    pub fn read_all(&self, requests: &[(PathBuf, ReadRequest, Probe)]) -> Vec<ReadOutcome> {
        let mut outcomes: Vec<Option<ReadOutcome>> = vec![None; requests.len()];
        let mut shared_deadline: Option<Instant> = None;
        let mut deferred: Vec<(usize, Begin)> = Vec::new();
        for (position, (path, request, probe)) in requests.iter().enumerate() {
            if shared_deadline.is_some() {
                deferred.push((position, self.begin(path, request, *probe)));
                continue;
            }
            let started = self.begin(path, request, *probe);
            let deadline = Instant::now() + self.budget;
            let result = self.finish(path, request, *probe, started, deadline);
            if result.is_none() {
                shared_deadline = Some(Instant::now() + self.budget);
            }
            outcomes[position] = Some(outcome(result));
        }
        if let Some(deadline) = shared_deadline {
            for (position, started) in deferred {
                let (path, request, probe) = &requests[position];
                outcomes[position] = Some(outcome(
                    self.finish(path, request, *probe, started, deadline),
                ));
            }
        }
        outcomes
            .into_iter()
            .map(|outcome| outcome.unwrap_or(ReadOutcome::Waiting))
            .collect()
    }

    /// Wait for a started read; `None` when it is not done by `deadline`.
    /// A read another caller started for a different request is waited for
    /// and then followed by this caller's own read.
    fn finish(
        &self,
        path: &Path,
        request: &ReadRequest,
        probe: Probe,
        mut started: Begin,
        deadline: Instant,
    ) -> Option<Result<SourceRead>> {
        loop {
            match started {
                Begin::Done(result) => return Some(result),
                Begin::Waiting => return None,
                Begin::Pending(slot) => {
                    let result = self.wait(path, &slot, deadline)?;
                    if slot.request == *request {
                        return Some(result);
                    }
                    started = self.begin(path, request, probe);
                }
            }
        }
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, Arc<Slot>>> {
        self.slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn begin(&self, path: &Path, request: &ReadRequest, probe: Probe) -> Begin {
        let mut slots = self.slots();
        if let Some(slot) = slots.get(path).cloned() {
            let state = slot_state(&slot);
            match &state.result {
                Some(result) => {
                    // A finished read is used once, and only for the same
                    // request against the same file; otherwise read again.
                    let usable = slot.request == *request
                        && probe.signature.is_some()
                        && slot.signature == probe.signature;
                    let result = result.clone();
                    drop(state);
                    slots.remove(path);
                    if usable {
                        return Begin::Done(result);
                    }
                }
                None if state.overdue => return Begin::Waiting,
                None => {
                    drop(state);
                    return Begin::Pending(slot);
                }
            }
        }
        let slot = Arc::new(Slot {
            request: request.clone(),
            signature: probe.signature,
            state: Mutex::new(SlotState {
                result: None,
                overdue: probe.placeholder,
            }),
            done: Condvar::new(),
        });
        let worker = Arc::clone(&slot);
        let reader = Arc::clone(&self.reader);
        let owned = path.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name("agendar-source-read".to_owned())
            .spawn(move || {
                let result = reader(&owned, &worker.request);
                let mut state = slot_state(&worker);
                state.result = Some(result);
                drop(state);
                worker.done.notify_all();
            });
        if let Err(error) = spawned {
            return Begin::Done(Err(error.into()));
        }
        slots.insert(path.to_path_buf(), Arc::clone(&slot));
        drop(slots);
        if probe.placeholder {
            // The background read asks the sync app for the content; nobody
            // waits for it now.
            return Begin::Waiting;
        }
        Begin::Pending(slot)
    }

    fn wait(&self, path: &Path, slot: &Arc<Slot>, deadline: Instant) -> Option<Result<SourceRead>> {
        let mut state = slot_state(slot);
        loop {
            if let Some(result) = &state.result {
                let result = result.clone();
                drop(state);
                let mut slots = self.slots();
                if slots
                    .get(path)
                    .is_some_and(|current| Arc::ptr_eq(current, slot))
                {
                    slots.remove(path);
                }
                return Some(result);
            }
            let now = Instant::now();
            if now >= deadline {
                state.overdue = true;
                return None;
            }
            state = slot
                .done
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }
}

fn slot_state(slot: &Slot) -> std::sync::MutexGuard<'_, SlotState> {
    slot.state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn outcome(result: Option<Result<SourceRead>>) -> ReadOutcome {
    match result {
        Some(Ok(read)) => ReadOutcome::Ready(Box::new(read)),
        Some(Err(error)) => ReadOutcome::Failed(error),
        None => ReadOutcome::Waiting,
    }
}

/// The retryable error for a source whose content is not on this computer.
pub fn waiting_error(path: &str) -> AgendarError {
    AgendarError::new(
        ErrorCode::Io,
        format!("The sync app has not downloaded {path} yet."),
    )
    .with_detail("source_waiting")
    .with_retryable(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_decision_reads_only_platform_bits() {
        assert!(!content_not_local(PlaceholderBits::default()));
        assert!(content_not_local(PlaceholderBits {
            st_flags: SF_DATALESS,
            file_attributes: 0,
        }));
        // Other BSD flags (hidden, immutable) do not mean "not local".
        assert!(!content_not_local(PlaceholderBits {
            st_flags: 0x8000 | 0x2,
            file_attributes: 0,
        }));
        for attribute in [
            FILE_ATTRIBUTE_OFFLINE,
            FILE_ATTRIBUTE_RECALL_ON_OPEN,
            FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        ] {
            assert!(content_not_local(PlaceholderBits {
                st_flags: 0,
                file_attributes: attribute | 0x20,
            }));
        }
        // Archive, hidden, read-only, pinned/unpinned alone: read normally.
        assert!(!content_not_local(PlaceholderBits {
            st_flags: 0,
            file_attributes: 0x20 | 0x2 | 0x1 | 0x0008_0000 | 0x0010_0000,
        }));
    }
}
