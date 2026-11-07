//! Harness adapters: capabilities and pull/hook delivery.
//!
//! The generic pull adapter, the Claude Code hook adapter and the Codex queue.
//! No adapter starts, installs or controls an agent process;
//! delivery means the packet was placed where the agent can read it, never that
//! the agent applied or acknowledged it.

pub mod claude;
pub mod codex;
pub mod generic;

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use generic::GenericAdapter;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::{Harness, InboxPacket};

/// Whether automatic wake is supported, proven missing, unknown, or available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticWake {
    Unsupported,
    PrerequisiteMissing,
    Unknown,
    Available,
}

/// Honest transport capabilities for one local harness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterCapabilities {
    pub harness: Harness,
    pub running: bool,
    pub ended_turn: bool,
    pub new_session: bool,
    pub queue_supported: bool,
    pub automatic_wake: AutomaticWake,
    pub detail: String,
}

/// Transport result. `Queued` means the harness accepted the message for the
/// addressed durable thread; it is not proof that the agent continued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportResult {
    Accepted,
    Queued(QueuedOutcome),
    PullRequired,
    Uncertain(String),
    Retryable(String),
    Unavailable(String),
}

/// Whether the queue call asked for an automatic resume and what the harness
/// said about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeStatus {
    WarningWaitingForResume,
    RequestedUnconfirmed,
}

/// Durable queue outcome persisted per packet/session attempt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuedOutcome {
    pub message_id: String,
    pub thread_id: String,
    pub resume_status: ResumeStatus,
}

/// One adapter can describe itself and deliver one packet to one local session.
pub trait Adapter: Send + Sync {
    fn capabilities(&self) -> AdapterCapabilities;
    fn deliver(&self, packet: &InboxPacket) -> Result<TransportResult>;
}

/// Capabilities for one harness without a live session.
pub fn capabilities_for(harness: Harness, running: bool) -> AdapterCapabilities {
    match harness {
        Harness::ClaudeCode => AdapterCapabilities {
            harness,
            running,
            ended_turn: false,
            new_session: true,
            queue_supported: false,
            automatic_wake: AutomaticWake::Unknown,
            detail: "Pull via PostToolUse context; Stop blocks once within its budget.".to_owned(),
        },
        Harness::Generic => AdapterCapabilities {
            harness,
            running,
            ended_turn: false,
            new_session: true,
            queue_supported: false,
            automatic_wake: AutomaticWake::Unsupported,
            detail: "Pull-only inbox; no push transport.".to_owned(),
        },
        Harness::Codex => AdapterCapabilities {
            harness,
            running,
            ended_turn: true,
            new_session: false,
            queue_supported: true,
            automatic_wake: AutomaticWake::Unknown,
            detail: "Durable queue to the original thread; automatic continuation needs an already-running local daemon.".to_owned(),
        },
        Harness::Pi
        | Harness::OpenCode
        | Harness::Cursor
        | Harness::Dsh
        | Harness::Agy
        | Harness::Gemini => AdapterCapabilities {
            harness,
            running,
            ended_turn: false,
            new_session: true,
            queue_supported: false,
            automatic_wake: AutomaticWake::Unsupported,
            detail: "Generic pull only; no terminal keystrokes or relay push.".to_owned(),
        },
        Harness::Herdr => AdapterCapabilities {
            harness,
            running,
            ended_turn: false,
            new_session: true,
            queue_supported: false,
            automatic_wake: AutomaticWake::Unsupported,
            detail: "Generic pull only; no terminal keystrokes or relay push.".to_owned(),
        },
    }
}
