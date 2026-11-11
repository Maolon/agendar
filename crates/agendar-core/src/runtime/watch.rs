//! Vault invalidation hints and the 60s reconciliation fallback.
//!
//! A filesystem watcher is only a hint that something may have changed; truth is
//! still read from the Vault by the index. `wait` subscribes before its first
//! query so an answer appended between the query and the wait is not missed, and
//! a slow reconciliation deadline catches missed platform notifications.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

use crate::error::Result;
use crate::vault::Vault;

/// Slow fallback reconciliation interval.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(60);

/// One wait outcome for a bounded receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The filesystem reported a change (or a possible change).
    Changed,
    /// The timeout elapsed with no change.
    TimedOut,
    /// The watcher channel closed; the caller must fall back to deadlines.
    Closed,
}

/// Whether a platform event can mean the Vault changed. Reads are not changes:
/// Linux reports the index's own reads of the Vault (open, read, close without
/// writing), which would otherwise wake `wait` again after every query. Closing
/// a file that was open for writing still counts, and so do errors and every
/// event the platform cannot classify.
fn is_change(event: &notify::Result<notify::Event>) -> bool {
    use notify::event::{AccessKind, AccessMode, EventKind};
    match event {
        Ok(event) => match event.kind {
            EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
            EventKind::Access(_) => false,
            _ => true,
        },
        Err(_) => true,
    }
}

/// The most paths one wait hands on; a larger burst is `unclear` instead.
const MAX_CHANGED_PATHS: usize = 4_096;

/// What the platform said changed during one wait. Still only a hint: a
/// caller that needs the truth reads the Vault.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// The paths the platform named, as it spelled them.
    pub paths: Vec<PathBuf>,
    /// An event named no path, asked for a rescan or was an error, or the
    /// burst was too large to keep: anything may have changed.
    pub unclear: bool,
}

impl Changes {
    fn add(&mut self, hint: Hint) {
        self.unclear |= hint.unclear;
        if self.paths.len().saturating_add(hint.paths.len()) > MAX_CHANGED_PATHS {
            self.unclear = true;
            return;
        }
        self.paths.extend(hint.paths);
    }
}

/// One platform event, reduced to what a caller may use.
struct Hint {
    paths: Vec<PathBuf>,
    unclear: bool,
}

fn hint(event: notify::Result<notify::Event>) -> Hint {
    match event {
        Ok(event) => Hint {
            unclear: event.need_rescan() || event.paths.is_empty(),
            paths: event.paths,
        },
        Err(_) => Hint {
            paths: Vec::new(),
            unclear: true,
        },
    }
}

/// A live recursive watch on the Vault root.
pub struct VaultWatch {
    receiver: Receiver<Hint>,
    _watcher: notify::RecommendedWatcher,
}

impl VaultWatch {
    /// Start a recursive watch on the Vault root. A platform refusal is returned
    /// so the caller can run with reconciliation only instead of failing the
    /// command.
    pub fn start(vault: &Vault) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                // Every change, including an error event, is only a hint.
                if is_change(&event) {
                    let _ = sender.send(hint(event));
                }
            })
            .map_err(|error| {
                crate::error::AgendarError::new(
                    crate::error::ErrorCode::Io,
                    format!("Cannot start the Vault file watcher: {error}."),
                )
                .with_detail("vault_watch")
            })?;
        watcher
            .watch(vault.root(), RecursiveMode::Recursive)
            .map_err(|error| {
                crate::error::AgendarError::new(
                    crate::error::ErrorCode::Io,
                    format!("Cannot watch the Vault: {error}."),
                )
                .with_detail("vault_watch")
            })?;
        Ok(Self {
            receiver,
            _watcher: watcher,
        })
    }

    /// Wait up to `timeout` for one change hint, coalescing a burst of platform
    /// events into a single wake-up.
    pub fn wait(&self, timeout: Duration) -> WaitOutcome {
        self.wait_changes(timeout).0
    }

    /// `wait`, and which paths the burst named.
    pub fn wait_changes(&self, timeout: Duration) -> (WaitOutcome, Changes) {
        let mut changes = Changes::default();
        let outcome = match self.receiver.recv_timeout(timeout) {
            Ok(first) => {
                changes.add(first);
                let drain_until = Instant::now() + Duration::from_millis(100);
                while let Ok(next) = self
                    .receiver
                    .recv_timeout(drain_until.saturating_duration_since(Instant::now()))
                {
                    changes.add(next);
                    if Instant::now() >= drain_until {
                        break;
                    }
                }
                WaitOutcome::Changed
            }
            Err(RecvTimeoutError::Timeout) => WaitOutcome::TimedOut,
            Err(RecvTimeoutError::Disconnected) => WaitOutcome::Closed,
        };
        (outcome, changes)
    }
}

#[cfg(test)]
mod tests {
    use notify::event::{AccessKind, AccessMode, CreateKind, DataChange, EventKind, ModifyKind};

    use super::{Changes, Hint, MAX_CHANGED_PATHS, hint, is_change};

    fn event(kind: EventKind) -> notify::Result<notify::Event> {
        Ok(notify::Event::new(kind))
    }

    #[test]
    fn reads_never_wake_a_wait_but_writes_and_errors_do() {
        for read in [
            AccessKind::Any,
            AccessKind::Read,
            AccessKind::Open(AccessMode::Read),
            AccessKind::Open(AccessMode::Write),
            AccessKind::Close(AccessMode::Read),
            AccessKind::Other,
        ] {
            assert!(!is_change(&event(EventKind::Access(read))), "{read:?}");
        }
        assert!(is_change(&event(EventKind::Access(AccessKind::Close(
            AccessMode::Write
        )))));
        assert!(is_change(&event(EventKind::Create(CreateKind::File))));
        assert!(is_change(&event(EventKind::Modify(ModifyKind::Data(
            DataChange::Content
        )))));
        assert!(is_change(&event(EventKind::Any)));
        assert!(is_change(&Err(notify::Error::generic("watch lost"))));
    }

    #[test]
    fn a_hint_keeps_the_named_paths_and_is_unclear_without_them() {
        let named =
            hint(Ok(notify::Event::new(EventKind::Create(CreateKind::File))
                .add_path("a/b.jsonl".into())));
        assert_eq!(named.paths, [std::path::PathBuf::from("a/b.jsonl")]);
        assert!(!named.unclear);
        assert!(hint(event(EventKind::Any)).unclear);
        assert!(hint(Err(notify::Error::generic("watch lost"))).unclear);
        let rescan = notify::Event::new(EventKind::Other)
            .add_path("a".into())
            .set_flag(notify::event::Flag::Rescan);
        assert!(hint(Ok(rescan)).unclear);

        // A burst too large to keep says so instead of growing without end.
        let mut changes = Changes::default();
        for _ in 0..=MAX_CHANGED_PATHS {
            changes.add(Hint {
                paths: vec!["a".into()],
                unclear: false,
            });
        }
        assert_eq!(changes.paths.len(), MAX_CHANGED_PATHS);
        assert!(changes.unclear);
    }

    /// The desktop matches these paths against the Vault root, so the
    /// platform must spell them as the canonical root does (a temporary
    /// directory on macOS is reached through a symlink).
    #[test]
    fn a_written_file_is_named_under_the_canonical_vault_root() {
        use crate::vault::writer::{SystemClock, UlidIdSource};

        let dir = tempfile::tempdir().expect("tempdir");
        let vault =
            crate::vault::Vault::init(dir.path(), &SystemClock, &mut UlidIdSource).expect("vault");
        let folder = vault.root().join("projects/demo/host");
        std::fs::create_dir_all(&folder).expect("folder");
        let watch = super::VaultWatch::start(&vault).expect("watch");
        let file = folder.join("operator.jsonl");
        std::fs::write(&file, b"{}\n").expect("write");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut named = false;
        while !named && std::time::Instant::now() < deadline {
            let (_, changes) = watch.wait_changes(std::time::Duration::from_secs(1));
            named = changes.paths.contains(&file);
        }
        assert!(
            named,
            "the watcher named the written file by its Vault path"
        );
    }
}
