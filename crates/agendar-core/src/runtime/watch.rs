//! Vault invalidation hints and the 60s reconciliation fallback.
//!
//! A filesystem watcher is only a hint that something may have changed; truth is
//! still read from the Vault by the index. `wait` subscribes before its first
//! query so an answer appended between the query and the wait is not missed, and
//! a slow reconciliation deadline catches missed platform notifications.

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

/// A live recursive watch on the Vault root.
pub struct VaultWatch {
    receiver: Receiver<()>,
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
                    let _ = sender.send(());
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
        match self.receiver.recv_timeout(timeout) {
            Ok(()) => {
                let drain_until = Instant::now() + Duration::from_millis(100);
                while let Ok(()) = self
                    .receiver
                    .recv_timeout(drain_until.saturating_duration_since(Instant::now()))
                {
                    if Instant::now() >= drain_until {
                        break;
                    }
                }
                WaitOutcome::Changed
            }
            Err(RecvTimeoutError::Timeout) => WaitOutcome::TimedOut,
            Err(RecvTimeoutError::Disconnected) => WaitOutcome::Closed,
        }
    }
}

#[cfg(test)]
mod tests {
    use notify::event::{AccessKind, AccessMode, CreateKind, DataChange, EventKind, ModifyKind};

    use super::is_change;

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
}
