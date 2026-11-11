//! The host's side of the upload mark: which of this host's own files to look
//! at after a change signal, when to read their upload flags, and telling the
//! UI when the line changes. The schedule itself is `crate::uploads::Tracker`.
//!
//! What a signal costs here: nothing unless it names one of this host's own
//! files (then one `stat` per named file). Every own folder is listed only at
//! start, on a Vault switch, as the safety net (`SAFETY_LISTING`), and after
//! every write this app itself commits (a rare, human action), whether or not
//! a watcher is running.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use agendar_core::runtime::Changes;
use serde_json::{Value, json};

use super::Host;
use crate::uploads::{
    Disk, OwnFile, OwnFiles, SAFETY_LISTING, Tracker, UploadReader, concern, platform_reader,
};

/// What the signals since the last step said.
#[derive(Default)]
struct Hints {
    /// The paths the watcher named.
    paths: BTreeSet<PathBuf>,
    /// A refresh ran: the safety-net listing may ride on it.
    refresh: bool,
    /// This app committed a write: its own files are listed, watcher or not.
    own_write: bool,
    /// The upload thread has something to do.
    wake: bool,
}

#[derive(Default)]
struct State {
    /// The Vault root the tracker belongs to.
    root: Option<PathBuf>,
    tracker: Tracker,
    /// When every own folder was last listed.
    listed: Option<Instant>,
}

pub(super) struct Uploads {
    reader: Mutex<Arc<dyn UploadReader>>,
    files: Mutex<Arc<dyn OwnFiles>>,
    state: Mutex<State>,
    hints: Mutex<Hints>,
    wake: Condvar,
}

impl Uploads {
    pub(super) fn new() -> Self {
        Self {
            reader: Mutex::new(platform_reader()),
            files: Mutex::new(Arc::new(Disk)),
            state: Mutex::new(State::default()),
            // The first tick lists once, for writes made just before launch.
            hints: Mutex::new(Hints {
                wake: true,
                ..Hints::default()
            }),
            wake: Condvar::new(),
        }
    }

    fn reader(&self) -> Arc<dyn UploadReader> {
        Arc::clone(
            &self
                .reader
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        )
    }

    fn files(&self) -> Arc<dyn OwnFiles> {
        Arc::clone(
            &self
                .files
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        )
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn hints(&self) -> MutexGuard<'_, Hints> {
        self.hints
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// What one step found in the folder.
enum Look {
    /// Every own file.
    All(Vec<OwnFile>),
    /// Only the named ones: as they are, and the ones gone.
    Named(Vec<OwnFile>, Vec<PathBuf>),
}

impl Host {
    /// Test seam: replace the platform's upload reader.
    #[doc(hidden)]
    pub fn set_upload_reader(&self, reader: Arc<dyn UploadReader>) {
        *self
            .uploads
            .reader
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = reader;
    }

    /// Test seam: replace the folder the own files are listed from.
    #[doc(hidden)]
    pub fn set_upload_files(&self, files: Arc<dyn OwnFiles>) {
        *self
            .uploads
            .files
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = files;
    }

    /// False where nothing can tell (Windows, Linux): no thread is started.
    pub fn uploads_supported(&self) -> bool {
        self.uploads.reader().supported()
    }

    /// This app committed a write. Its own files are listed at once: the
    /// commit does not say which file it wrote, and it must not depend on a
    /// watcher to name it.
    pub(super) fn uploads_committed(&self) {
        if !self.uploads_supported() {
            return;
        }
        let mut hints = self.uploads.hints();
        hints.own_write = true;
        hints.wake = true;
        self.uploads.wake.notify_all();
    }

    /// A refresh is running; `changes` is what the watcher named, if it was
    /// the watcher. Nothing is listed or looked at here.
    pub(super) fn uploads_signal(&self, changes: Option<&Changes>) {
        if !self.uploads_supported() {
            return;
        }
        let mut hints = self.uploads.hints();
        hints.refresh = true;
        if let Some(changes) = changes {
            hints.paths.extend(changes.paths.iter().cloned());
        }
        hints.wake = true;
        self.uploads.wake.notify_all();
    }

    /// Sleep until a signal arrives, a read is due, time alone may change
    /// the line, or `timeout` passes.
    pub fn wait_for_uploads(&self, timeout: Duration) {
        let now = Instant::now();
        let (due, remark) = {
            let state = self.uploads.state();
            (
                state.tracker.next_due(),
                state.tracker.next_remark(self.clock.now()),
            )
        };
        let wait = due
            .map(|due| due.saturating_duration_since(now))
            .into_iter()
            .chain(remark)
            .fold(timeout, Duration::min);
        let hints = self.uploads.hints();
        let _unused = self
            .uploads
            .wake
            .wait_timeout_while(hints, wait, |hints| !hints.wake && !self.is_shutdown());
    }

    pub fn uploads_tick(&self) {
        self.uploads_tick_at(Instant::now());
    }

    /// One step: look at the own files the signals named (or list them all,
    /// at start and as the safety net), read the flags that are due, and emit
    /// `agendar://upload` when the line changed.
    #[doc(hidden)]
    pub fn uploads_tick_at(&self, now: Instant) {
        let reader = self.uploads.reader();
        if !reader.supported() || self.is_shutdown() {
            return;
        }
        let hints = std::mem::take(&mut *self.uploads.hints());
        let vault = {
            let inner = self.lock();
            inner
                .service
                .as_ref()
                .map(|service| (service.vault().root().to_path_buf(), service.host().clone()))
        };
        let wall = self.clock.now();
        let Some((root, host)) = vault else {
            *self.uploads.state() = State::default();
            return;
        };
        let list_all = {
            let mut state = self.uploads.state();
            if state.root.as_ref() != Some(&root) {
                *state = State {
                    root: Some(root.clone()),
                    ..State::default()
                };
            }
            let stale = state
                .listed
                .is_none_or(|listed| now.saturating_duration_since(listed) >= SAFETY_LISTING);
            let all = state.listed.is_none() || hints.own_write || (hints.refresh && stale);
            if all {
                state.listed = Some(now);
            }
            all
        };
        // The folder and the sync daemon may be slow: no lock is held.
        let files = self.uploads.files();
        let look = if list_all {
            Some(Look::All(files.all(&root, &host, wall)))
        } else {
            let concern = concern(&root, &host, &hints.paths);
            (!concern.is_empty()).then(|| {
                let mut seen: Vec<_> = concern
                    .projects
                    .iter()
                    .flat_map(|project| files.project(&root, project, &host, wall))
                    .collect();
                let mut gone = Vec::new();
                for path in concern.files {
                    match files.one(&path, wall) {
                        Some(file) => seen.push(file),
                        None => gone.push(path),
                    }
                }
                Look::Named(seen, gone)
            })
        };
        let due = {
            let mut state = self.uploads.state();
            if state.root.as_ref() != Some(&root) {
                return;
            }
            match look {
                Some(Look::All(files)) => state.tracker.changed(files, now, wall),
                Some(Look::Named(files, gone)) => state.tracker.touched(files, &gone, now),
                None => {}
            }
            state.tracker.take_due(now)
        };
        let read: Vec<_> = due
            .into_iter()
            .map(|path| {
                let flag = reader.read(&path);
                (path, flag)
            })
            .collect();
        let view = {
            let mut state = self.uploads.state();
            if state.root.as_ref() != Some(&root) {
                return;
            }
            // With no read due, time alone may still change the line.
            let moved = if read.is_empty() {
                state.tracker.remark(wall)
            } else {
                state.tracker.apply(&read, now, wall)
            };
            if !moved {
                return;
            }
            state.tracker.mark().view()
        };
        self.ui
            .emit_upload(&json!({"v": 1, "upload": view.unwrap_or(Value::Null)}));
    }

    /// `local.upload` of the snapshot: absent while nothing waits.
    pub(super) fn upload_view(&self) -> Option<Value> {
        self.uploads.state().tracker.mark().view()
    }
}
