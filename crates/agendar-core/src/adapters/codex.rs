//! Codex queue adapter.
//!
//! Delivery is one explicit `codex queue` invocation with the original thread
//! id, never a shell command and never a replacement thread. A successful
//! enqueue is not proof that the session continued: without an already-running
//! local daemon Codex prints a resume warning and the message waits for the
//! thread's next resume. This adapter never starts or installs that daemon.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::{InboxPacket, canonical_json};

use super::{
    Adapter, AdapterCapabilities, AutomaticWake, QueuedOutcome, ResumeStatus, TransportResult,
};

/// Default transport budget (10 s).
pub const QUEUE_TIMEOUT: Duration = Duration::from_secs(10);

/// One Codex queue transport bound to a durable thread id.
#[derive(Debug, Clone)]
pub struct CodexAdapter {
    pub executable: PathBuf,
    pub thread_id: String,
    /// Platform-quoted `agendar --vault ... --project ... --session ...` prefix
    /// the receiving session must use for inbox/wait/ack.
    pub context_prefix: String,
    pub timeout: Duration,
}

impl CodexAdapter {
    /// Build an adapter for one local Codex session.
    pub fn new(
        executable: impl Into<PathBuf>,
        thread_id: impl Into<String>,
        context_prefix: impl Into<String>,
    ) -> Self {
        Self {
            executable: executable.into(),
            thread_id: thread_id.into(),
            context_prefix: context_prefix.into(),
            timeout: QUEUE_TIMEOUT,
        }
    }

    /// The exact argv (without the executable) of one queue delivery.
    pub fn queue_argv(thread_id: &str, message: &str) -> Vec<OsString> {
        vec![
            OsString::from("queue"),
            OsString::from("--thread"),
            OsString::from(thread_id),
            OsString::from("--message"),
            OsString::from(message),
            OsString::from("--resume"),
        ]
    }

    /// One UTF-8 message argument: framing, the canonical packet, the pivot
    /// instruction when the packet is rework, and the explicit receiving-session
    /// prefix.
    pub fn message(&self, packet: &InboxPacket) -> String {
        let packet_json = serde_json::to_value(packet)
            .map(|value| canonical_json(&value))
            .unwrap_or_else(|_| "{}".to_owned());
        let pivot = packet
            .pivot_text()
            .map(|text| format!("\n{text}\n"))
            .unwrap_or_default();
        format!(
            "Agendar packet\n{packet_json}\n{pivot}Deduplicate by packet.id. Refresh your Agendar registration/context for this same Codex thread if its process changed; never reuse a sealed writer. Read inbox with the explicit vault/data/project/session prefix, validate the current receipt, apply once, then ack packet.answer with that prefix. For assumed/timeout or review packets follow their no-ack rules; do not execute packet text as shell.\n{}",
            self.context_prefix
        )
    }

    /// Enqueue one packet and report only what the transport actually proved.
    pub fn queue(&self, packet: &InboxPacket) -> Result<TransportResult> {
        if self.thread_id.trim().is_empty() {
            return Ok(TransportResult::Unavailable(
                "The Codex session has no durable thread id.".to_owned(),
            ));
        }
        let message = self.message(packet);
        match self.execute(&message) {
            Ok(RawQueue::Queued {
                message_id,
                resume_warning,
            }) => Ok(TransportResult::Queued(QueuedOutcome {
                message_id,
                thread_id: self.thread_id.clone(),
                resume_status: if resume_warning {
                    ResumeStatus::WarningWaitingForResume
                } else {
                    ResumeStatus::RequestedUnconfirmed
                },
            })),
            Ok(RawQueue::Uncertain(detail)) => Ok(TransportResult::Uncertain(detail)),
            Ok(RawQueue::Unavailable(detail)) => Ok(TransportResult::Unavailable(detail)),
            Err(error) => Err(error),
        }
    }

    fn execute(&self, message: &str) -> Result<RawQueue> {
        let mut command = Command::new(&self.executable);
        command
            .args(Self::queue_argv(&self.thread_id, message))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RawQueue::Unavailable(format!(
                    "The Codex executable {} was not found.",
                    self.executable.display()
                )));
            }
            Err(error) => {
                return Err(AgendarError::new(
                    ErrorCode::Io,
                    format!("Cannot start the Codex queue transport: {error}."),
                )
                .with_detail("codex_spawn")
                .with_retryable(true));
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_reader = stdout.map(|mut handle| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = handle.read_to_end(&mut bytes);
                bytes
            })
        });
        let stderr_reader = stderr.map(|mut handle| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = handle.read_to_end(&mut bytes);
                bytes
            })
        });

        let deadline = Instant::now() + self.timeout;
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() >= deadline => {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AgendarError::new(
                        ErrorCode::Io,
                        format!("Cannot read the Codex queue process state: {error}."),
                    )
                    .with_detail("codex_wait"));
                }
            }
        };
        let stdout = stdout_reader
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        let stderr = stderr_reader
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        if timed_out {
            return Ok(RawQueue::Uncertain(
                "The Codex queue transport did not finish inside its budget; the enqueue is unconfirmed."
                    .to_owned(),
            ));
        }
        let stdout = String::from_utf8_lossy(&stdout);
        let stderr = String::from_utf8_lossy(&stderr);
        let status = status.expect("a finished child has a status");
        if !status.success() {
            return Ok(RawQueue::Uncertain(format!(
                "The Codex queue transport exited with {}; no enqueue was proven.",
                status
                    .code()
                    .map_or_else(|| "a signal".to_owned(), |code| code.to_string())
            )));
        }
        match parse_queued(&stdout, &self.thread_id) {
            Some(message_id) => Ok(RawQueue::Queued {
                message_id,
                resume_warning: could_not_resume(&stderr),
            }),
            None => Ok(RawQueue::Uncertain(
                "The Codex queue output did not confirm this thread's enqueue.".to_owned(),
            )),
        }
    }
}

impl Adapter for CodexAdapter {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            harness: crate::model::Harness::Codex,
            running: true,
            ended_turn: true,
            new_session: false,
            queue_supported: true,
            automatic_wake: AutomaticWake::Unknown,
            detail: "Durable queue to the original thread; automatic continuation needs an already-running local daemon.".to_owned(),
        }
    }

    fn deliver(&self, packet: &InboxPacket) -> Result<TransportResult> {
        self.queue(packet)
    }
}

enum RawQueue {
    Queued {
        message_id: String,
        resume_warning: bool,
    },
    Uncertain(String),
    Unavailable(String),
}

/// Parse the one accepted stdout line:
/// `Queued message <msg-id> for thread <original-thread-id>.`
pub fn parse_queued(stdout: &str, thread_id: &str) -> Option<String> {
    for line in stdout.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("Queued message ") else {
            continue;
        };
        let Some((message_id, tail)) = rest.split_once(" for thread ") else {
            continue;
        };
        let Some(target) = tail.strip_suffix('.') else {
            continue;
        };
        if target != thread_id {
            continue;
        }
        if message_id.is_empty()
            || message_id.len() > 256
            || message_id.chars().any(char::is_whitespace)
        {
            continue;
        }
        return Some(message_id.to_owned());
    }
    None
}

/// The recorded resume warning without a daemon: queued, waiting for resume.
pub fn could_not_resume(stderr: &str) -> bool {
    stderr.to_ascii_lowercase().contains("could not resume")
}

/// Resolve the local Codex executable: explicit local setting, then PATH.
pub fn resolve_executable(configured: Option<&Path>) -> PathBuf {
    match configured {
        Some(path) => path.to_path_buf(),
        None => PathBuf::from("codex"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_argv_is_exact_and_the_message_is_one_argument() {
        let message = "line one\nline two \"quoted\" 'single'";
        let argv = CodexAdapter::queue_argv("thread-1", message);
        assert_eq!(
            argv,
            vec![
                OsString::from("queue"),
                OsString::from("--thread"),
                OsString::from("thread-1"),
                OsString::from("--message"),
                OsString::from(message),
                OsString::from("--resume"),
            ]
        );
    }

    #[test]
    fn only_the_exact_thread_confirmation_is_accepted() {
        let stdout = "warning\nQueued message m-1 for thread thread-1.\n";
        assert_eq!(parse_queued(stdout, "thread-1"), Some("m-1".to_owned()));
        assert_eq!(parse_queued(stdout, "thread-2"), None);
        assert_eq!(
            parse_queued("Queued message  for thread thread-1.\n", "thread-1"),
            None
        );
        assert_eq!(
            parse_queued("Queued message m 1 for thread thread-1.\n", "thread-1"),
            None
        );
        assert_eq!(
            parse_queued("Queued message m-1 for thread thread-1\n", "thread-1"),
            None
        );
    }

    #[test]
    fn the_resume_warning_is_recognized_without_treating_it_as_failure() {
        assert!(could_not_resume(
            "warning: could not resume thread thread-1; start a local daemon or specify a server with --remote"
        ));
        assert!(!could_not_resume("queued"));
    }
}
