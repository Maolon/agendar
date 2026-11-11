//! Whether this computer's own Vault writes have left it yet.
//!
//! Local display state only: nothing here is written to the Vault. After this
//! host writes one of its own files (a writer file or an attachment), the sync
//! folder's upload flag of that file is read once, then re-read on a doubling
//! delay until it says uploaded. Reading the flag makes the sync daemon work,
//! so it is never polled at a fixed rate, never read for another host's
//! files, and not read at all while everything is uploaded.
//!
//! Own files are told from other hosts' by the Vault layout alone:
//! `projects/<project>/<host>/*.jsonl` and `projects/<project>/<host>/blobs/*`.
//! A change signal that names paths costs one `stat` per own file it names
//! and nothing for anything else; every own folder is listed only at start
//! and as a safety net.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use agendar_core::model::{HostId, UtcTime};
use serde_json::{Value, json};

#[cfg(target_os = "macos")]
mod icloud;

/// The first re-read after a change, doubled after every read.
pub const FIRST_DELAY: Duration = Duration::from_secs(2);
/// The longest wait between two re-reads.
pub const MAX_DELAY: Duration = Duration::from_secs(30);
/// How long after the last own write the re-reads go on.
pub const GIVE_UP: Duration = Duration::from_secs(600);
/// After giving up, one look this often, so the mark cannot outlive the upload.
pub const STUCK_RECHECK: Duration = Duration::from_secs(300);
/// A normal upload takes 3-6 s: no line before a write has waited this long.
pub const QUIET_S: i64 = 15;
/// The stronger line: this long after the latest write of a waiting file...
pub const LATE_AFTER_S: i64 = 120;
/// ...or this long after its first write not seen uploaded, so a file that is
/// written again and again still turns late.
pub const LATE_FIRST_S: i64 = 600;
/// At launch, only files written this recently are looked at.
pub const SEED_WINDOW_S: i64 = 600;
/// The safety net: every own folder is listed again no sooner than this after
/// the last listing, and only when a refresh is running anyway.
pub const SAFETY_LISTING: Duration = Duration::from_secs(300);

/// What the sync folder says about one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadFlag {
    Uploaded,
    Waiting,
    /// Nothing can tell: not a synced folder, or not this platform.
    Unknown,
}

/// The platform seam. Tests use a fake.
pub trait UploadReader: Send + Sync {
    /// False where nothing can ever tell; the host then does no work at all.
    fn supported(&self) -> bool;
    fn read(&self, file: &Path) -> UploadFlag;
}

/// The reader of the platform this build runs on.
pub fn platform_reader() -> std::sync::Arc<dyn UploadReader> {
    #[cfg(target_os = "macos")]
    {
        std::sync::Arc::new(icloud::ICloudReader)
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::sync::Arc::new(NoReader)
    }
}

/// Windows and Linux: no upload state is known.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoReader;

impl UploadReader for NoReader {
    fn supported(&self) -> bool {
        false
    }

    fn read(&self, _file: &Path) -> UploadFlag {
        UploadFlag::Unknown
    }
}

/// One of this host's own files (a writer file or an attachment) as the
/// folder lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnFile {
    pub path: PathBuf,
    pub len: u64,
    pub modified: SystemTime,
    /// `modified` on the host's wall clock, for "since 14:02".
    pub written: UtcTime,
}

/// The folder seam: names and metadata of this host's own files. No file is
/// opened and no other host's folder is entered. Tests count the calls.
pub trait OwnFiles: Send + Sync {
    /// Every own file of the Vault: one `read_dir` of `projects`, then per
    /// project one of this host's folder and one of its `blobs`.
    fn all(&self, root: &Path, host: &HostId, wall: UtcTime) -> Vec<OwnFile>;
    /// The own files of one project folder.
    fn project(&self, root: &Path, project: &str, host: &HostId, wall: UtcTime) -> Vec<OwnFile>;
    /// One own file; `None` when it is not there (or not a regular file).
    fn one(&self, file: &Path, wall: UtcTime) -> Option<OwnFile>;
}

/// The real folder.
#[derive(Debug, Default, Clone, Copy)]
pub struct Disk;

impl OwnFiles for Disk {
    fn all(&self, root: &Path, host: &HostId, wall: UtcTime) -> Vec<OwnFile> {
        let mut files = Vec::new();
        let Ok(projects) = std::fs::read_dir(root.join("projects")) else {
            return files;
        };
        for project in projects.flatten() {
            list_folder(&project.path().join(host.as_str()), wall, &mut files);
        }
        files
    }

    fn project(&self, root: &Path, project: &str, host: &HostId, wall: UtcTime) -> Vec<OwnFile> {
        let mut files = Vec::new();
        let folder = root.join("projects").join(project).join(host.as_str());
        list_folder(&folder, wall, &mut files);
        files
    }

    fn one(&self, file: &Path, wall: UtcTime) -> Option<OwnFile> {
        stat(file.to_path_buf(), wall, SystemTime::now())
    }
}

const BLOBS: &str = "blobs";

/// A writer file by its name.
fn is_writer_name(name: &str) -> bool {
    Path::new(name).extension().and_then(|value| value.to_str()) == Some("jsonl")
}

/// An attachment by its name: anything in `blobs` but a hidden file (the sync
/// folder keeps its own there).
fn is_blob_name(name: &str) -> bool {
    !name.starts_with('.')
}

/// One host folder `projects/<project>/<host>`: its writer files and the
/// attachments in its `blobs`.
fn list_folder(folder: &Path, wall: UtcTime, files: &mut Vec<OwnFile>) {
    let now = SystemTime::now();
    let mut list = |folder: &Path, wanted: fn(&str) -> bool| {
        let Ok(entries) = std::fs::read_dir(folder) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_str().is_some_and(wanted) {
                files.extend(stat(entry.path(), wall, now));
            }
        }
    };
    list(folder, is_writer_name);
    list(&folder.join(BLOBS), is_blob_name);
}

fn stat(path: PathBuf, wall: UtcTime, now: SystemTime) -> Option<OwnFile> {
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let modified = metadata.modified().ok()?;
    let age = now
        .duration_since(modified)
        .map_or(0, |age| i64::try_from(age.as_millis()).unwrap_or(i64::MAX));
    Some(OwnFile {
        path,
        len: metadata.len(),
        modified,
        written: wall.add_millis(age.saturating_neg()),
    })
}

/// What a change signal's paths can mean for this host's own files. Worked
/// out from the names alone: no file system call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Concern {
    /// Own files the signal named: one `stat` each.
    pub files: BTreeSet<PathBuf>,
    /// Project folders whose own host folder (or its `blobs`) was named
    /// itself: that one folder is listed.
    pub projects: BTreeSet<String>,
    /// A path was not under the Vault root as this host spells it: only the
    /// safety-net listing can tell.
    pub unclear: bool,
}

impl Concern {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.projects.is_empty()
    }
}

/// Sort the paths a change signal named. Another host's path, and anything
/// that is not a writer file or an attachment, is nothing here.
pub fn concern<'a>(
    root: &Path,
    host: &HostId,
    paths: impl IntoIterator<Item = &'a PathBuf>,
) -> Concern {
    let mut concern = Concern::default();
    for path in paths {
        let Ok(relative) = path.strip_prefix(root) else {
            concern.unclear = true;
            continue;
        };
        let Some(parts) = relative
            .components()
            .map(|part| part.as_os_str().to_str())
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let ["projects", project, owner, rest @ ..] = parts.as_slice() else {
            continue;
        };
        if *owner != host.as_str() {
            continue;
        }
        match rest {
            [] | [BLOBS] => {
                concern.projects.insert((*project).to_owned());
            }
            [name] if is_writer_name(name) => {
                concern.files.insert(path.clone());
            }
            [BLOBS, name] if is_blob_name(name) => {
                concern.files.insert(path.clone());
            }
            _ => {}
        }
    }
    concern
}

/// What the desktop shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UploadMark {
    /// Everything is uploaded, or nothing can tell: no line.
    #[default]
    Clear,
    Waiting {
        since: UtcTime,
    },
    Late {
        since: UtcTime,
    },
}

impl UploadMark {
    /// `local.upload` of the snapshot; absent when there is no line.
    pub fn view(&self) -> Option<Value> {
        match self {
            Self::Clear => None,
            Self::Waiting { since } => Some(json!({"state": "waiting", "since": since})),
            Self::Late { since } => Some(json!({"state": "late", "since": since})),
        }
    }
}

/// One written file that has not been seen uploaded.
#[derive(Debug, Clone, Copy)]
struct Pending {
    /// The first write not seen uploaded: what the line counts from.
    first: UtcTime,
    /// The latest write.
    written: UtcTime,
    /// When the tracker noticed it.
    noticed: Instant,
    /// A read has said "not uploaded": only then does the line show.
    waiting: bool,
}

/// The schedule and the state, with no clock and no I/O of its own.
#[derive(Debug, Default)]
pub struct Tracker {
    /// Size and time of every own file at the last listing; `None` before the
    /// first one.
    known: Option<BTreeMap<PathBuf, (u64, SystemTime)>>,
    /// Files written and not yet seen uploaded.
    pending: BTreeMap<PathBuf, Pending>,
    delay: Duration,
    next: Option<Instant>,
    give_up: Option<Instant>,
    last_read: Option<Instant>,
    stuck: bool,
    mark: UploadMark,
}

impl Tracker {
    pub fn mark(&self) -> &UploadMark {
        &self.mark
    }

    /// When the next read is due; `None` while there is nothing to do.
    pub fn next_due(&self) -> Option<Instant> {
        self.next
    }

    /// `files` is a fresh listing of every own file. A file whose size or
    /// time moved was written by this host and joins the wait. The first
    /// listing only takes files written within `SEED_WINDOW_S`.
    pub fn changed(&mut self, files: Vec<OwnFile>, now: Instant, wall: UtcTime) {
        let first = self.known.is_none();
        let known = self.known.take().unwrap_or_default();
        let mut seen = BTreeMap::new();
        let mut fresh = false;
        for file in files {
            let stamp = (file.len, file.modified);
            let recent =
                wall.millis().saturating_sub(file.written.millis()) <= SEED_WINDOW_S * 1000;
            if known.get(&file.path) != Some(&stamp) && (!first || recent) {
                self.wait_for(&file, now);
                fresh = true;
            }
            seen.insert(file.path, stamp);
        }
        self.pending.retain(|path, _| seen.contains_key(path));
        self.known = Some(seen);
        if fresh {
            self.schedule(now);
        }
    }

    /// Some own files were looked at alone, because a change signal named
    /// them: `files` as they are now, `gone` the ones no longer there. The
    /// rest of what is known stays as it was.
    pub fn touched(&mut self, files: Vec<OwnFile>, gone: &[PathBuf], now: Instant) {
        let mut fresh = false;
        for file in files {
            let stamp = (file.len, file.modified);
            let known = self.known.get_or_insert_with(BTreeMap::new);
            if known.get(&file.path) != Some(&stamp) {
                known.insert(file.path.clone(), stamp);
                self.wait_for(&file, now);
                fresh = true;
            }
        }
        for path in gone {
            if let Some(known) = self.known.as_mut() {
                known.remove(path);
            }
            self.pending.remove(path);
        }
        if fresh {
            self.schedule(now);
        }
    }

    /// `file` was written: it waits, counted from its first write not seen
    /// uploaded.
    fn wait_for(&mut self, file: &OwnFile, now: Instant) {
        let old = self.pending.get(&file.path);
        let waiting = old.is_some_and(|old| old.waiting);
        let first = old.map_or(file.written, |old| old.first);
        self.pending.insert(
            file.path.clone(),
            Pending {
                first,
                written: file.written,
                noticed: now,
                waiting,
            },
        );
    }

    /// A write was noticed: start the reads, or join the ones running.
    fn schedule(&mut self, now: Instant) {
        self.give_up = Some(now + GIVE_UP);
        if self.next.is_some() && !self.stuck {
            // Already waiting on a schedule: the new write joins it. A burst
            // of writes must not turn into a read per write.
            return;
        }
        self.stuck = false;
        self.delay = FIRST_DELAY;
        // Read on change, but never twice within the first delay.
        self.next = Some(
            self.last_read
                .map_or(now, |last| (last + FIRST_DELAY).max(now)),
        );
    }

    /// The files to read now. Empty when no read is due.
    pub fn take_due(&mut self, now: Instant) -> Vec<PathBuf> {
        if self.pending.is_empty() {
            // The waiting files are gone from the folder: nothing to ask.
            self.next = None;
            self.give_up = None;
            self.stuck = false;
        }
        if self.next.is_none_or(|next| next > now) {
            return Vec::new();
        }
        self.pending.keys().cloned().collect()
    }

    /// What the reads of `take_due` said. True when the mark changed.
    pub fn apply(&mut self, read: &[(PathBuf, UploadFlag)], now: Instant, wall: UtcTime) -> bool {
        self.last_read = Some(now);
        for (path, flag) in read {
            let Some(entry) = self.pending.get_mut(path) else {
                continue;
            };
            match flag {
                UploadFlag::Waiting => entry.waiting = true,
                UploadFlag::Unknown => {
                    self.pending.remove(path);
                }
                // Right after an append the flag still says uploaded (it is
                // the previous version's): believe it only after the first
                // delay.
                UploadFlag::Uploaded => {
                    if now.saturating_duration_since(entry.noticed) >= FIRST_DELAY {
                        self.pending.remove(path);
                    }
                }
            }
        }
        if self.pending.is_empty() {
            self.next = None;
            self.give_up = None;
            self.stuck = false;
        } else if self.stuck || self.give_up.is_some_and(|limit| now >= limit) {
            self.stuck = true;
            self.next = Some(now + STUCK_RECHECK);
        } else {
            self.next = Some(now + self.delay);
            self.delay = (self.delay * 2).min(MAX_DELAY);
        }
        self.remark(wall)
    }

    /// Work the line out again for `wall`, with no read: time alone turns
    /// nothing into "not uploaded yet" and that into the late line. True when
    /// the mark changed.
    pub fn remark(&mut self, wall: UtcTime) -> bool {
        let age = |time: UtcTime| wall.millis().saturating_sub(time.millis());
        let waiting = || self.pending.values().filter(|entry| entry.waiting);
        let mark = match waiting().map(|entry| entry.first).min() {
            None => UploadMark::Clear,
            Some(since) => {
                let late = self.stuck
                    || age(since) >= LATE_FIRST_S * 1000
                    || waiting().any(|entry| age(entry.written) >= LATE_AFTER_S * 1000);
                if late {
                    UploadMark::Late { since }
                } else if age(since) >= QUIET_S * 1000 {
                    UploadMark::Waiting { since }
                } else {
                    UploadMark::Clear
                }
            }
        };
        let moved = mark != self.mark;
        self.mark = mark;
        moved
    }

    /// How long from `wall` until time alone may change the line; `None` when
    /// it cannot.
    pub fn next_remark(&self, wall: UtcTime) -> Option<Duration> {
        self.pending
            .values()
            .filter(|entry| entry.waiting)
            .flat_map(|entry| {
                [
                    entry.first.add_seconds(QUIET_S),
                    entry.written.add_seconds(LATE_AFTER_S),
                    entry.first.add_seconds(LATE_FIRST_S),
                ]
            })
            .filter_map(|at| u64::try_from(at.millis().saturating_sub(wall.millis())).ok())
            .filter(|millis| *millis > 0)
            .min()
            .map(Duration::from_millis)
    }
}

#[cfg(test)]
mod tests;
