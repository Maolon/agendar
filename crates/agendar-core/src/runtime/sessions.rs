//! Local runtime journal: session registrations, operation idempotency,
//! wait leases and delivery attempts.
//!
//! Everything here lives in `<data>/vaults/<VaultId>/runtime.json`, never in the
//! Vault. Every read-modify-write goes through one cross-process lock, rereads
//! the latest committed file, applies a bounded patch, increments the revision
//! and atomically replaces the file. A caller never caches and overwrites a
//! stale whole journal, so two concurrent registrations cannot lose each other.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessesToUpdate, System};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::{EventId, Harness, ProjectSlug, SessionId, Sha256, UtcTime};
use crate::runtime::{LocalPaths, acquire_lock_until, read_json_value, write_json_atomic};
use crate::vault::{Clock, EventDraft, SystemClock, Vault};

/// Version of the runtime journal.
pub const RUNTIME_VERSION: u8 = 1;

/// Default wait lease lifetime.
pub const WAIT_LEASE_TTL_S: u64 = 90;

/// State of one service operation reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    /// Intent and IDs are durable; the append or its result may be incomplete.
    Reserved,
    /// The result is known and returned to identical retries.
    Committed,
}

/// One idempotent/request-id operation reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRecord {
    pub command: String,
    pub project: ProjectSlug,
    pub session: SessionId,
    pub writer: String,
    pub input_digest: Sha256,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved_event: Option<EventId>,
    pub draft: EventDraft,
    pub status: OperationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    pub updated_at: UtcTime,
}

/// One local harness session registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSession {
    pub project: ProjectSlug,
    pub harness: Harness,
    pub harness_session_id: String,
    pub pid: u32,
    /// OS process start identity in seconds since the Unix epoch.
    pub process_start: u64,
    /// Canonical local repo root (local only; never a Vault field).
    pub repo_root: String,
    pub registered_at: UtcTime,
    /// The one start pulse is durable.
    pub started: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_event: Option<EventId>,
    /// Reserved ID for the start pulse, reused after a crash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_reserved: Option<EventId>,
    /// A lost fact was recorded and the writer sealed.
    pub lost: bool,
    /// The agent writer was sealed at session end and may never be appended to.
    pub sealed: bool,
    /// The harness process for this registration exited but the session stays
    /// resumable by its durable harness id (a Codex thread outlives the process
    /// of each turn). No loss fact was recorded and the writer is still open.
    #[serde(default)]
    pub resumable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_event: Option<EventId>,
    /// Reserved ID for the one loss event, reused after a crash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_reserved: Option<EventId>,
    /// The last human plan edit this session was shown in `inbox`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agenda_seen: Option<EventId>,
    /// Reminders a hook already put in this session's context (newest last,
    /// at most [`MAX_REMINDED`]), so each one is shown once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reminded: Vec<String>,
}

/// How many shown reminders a session remembers.
pub const MAX_REMINDED: usize = 64;

impl LocalSession {
    /// True when this registration belongs to the same live process identity.
    pub fn matches_identity(
        &self,
        project: &ProjectSlug,
        harness: Harness,
        harness_session_id: &str,
        pid: u32,
        process_start: u64,
    ) -> bool {
        !self.sealed
            && !self.lost
            && &self.project == project
            && self.harness == harness
            && self.harness_session_id == harness_session_id
            && self.pid == pid
            && self.process_start == process_start
    }
}

/// One delivered packet attempt. At-least-once transport is deduplicated by the
/// packet ID; a repeated inbox read never marks the same packet again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryAttempt {
    pub packet: Sha256,
    pub session: SessionId,
    pub outcome: String,
    pub delivered_at: UtcTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<EventId>,
    /// Reserved event ID, reused after a crash without a duplicate delivery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved: Option<EventId>,
    /// Durable queue confirmation for a Codex thread. Persisted before the
    /// steward `delivered` event so a restart never re-enqueues the same packet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<crate::adapters::QueuedOutcome>,
}

/// A local wait lease. It annotates "waiting now" for this host only and never
/// enters the Vault or ranking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitLease {
    pub session: SessionId,
    pub ask: EventId,
    pub updated_at: UtcTime,
    pub ttl_s: u64,
}

/// Versioned local runtime journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeState {
    pub v: u8,
    pub revision: u64,
    pub sessions: BTreeMap<SessionId, LocalSession>,
    pub approvals: BTreeMap<String, serde_json::Value>,
    pub delivery_obligations: BTreeMap<String, serde_json::Value>,
    pub delivery_attempts: BTreeMap<String, DeliveryAttempt>,
    pub wait_leases: BTreeMap<String, WaitLease>,
    pub operations: BTreeMap<String, OperationRecord>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            v: RUNTIME_VERSION,
            revision: 0,
            sessions: BTreeMap::new(),
            approvals: BTreeMap::new(),
            delivery_obligations: BTreeMap::new(),
            delivery_attempts: BTreeMap::new(),
            wait_leases: BTreeMap::new(),
            operations: BTreeMap::new(),
        }
    }
}

impl RuntimeState {
    /// Read the committed runtime journal without creating it.
    pub fn load(local: &LocalPaths, vault: &Vault) -> Result<Self> {
        let path = local.runtime_path(vault.id());
        let Some(value) = read_json_value(&path)? else {
            return Ok(Self::default());
        };
        let version = value
            .get("v")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                AgendarError::validation_at(
                    "/runtime",
                    "runtime_missing_version",
                    "runtime.json has no version field.",
                )
            })?;
        if version != u64::from(RUNTIME_VERSION) {
            return Err(AgendarError::new(
                ErrorCode::Unsupported,
                format!("Runtime journal version {version} is not supported."),
            )
            .with_detail("runtime_version")
            .with_hint("Use the Agendar version that created this local state."));
        }
        let runtime: RuntimeState = serde_json::from_value(value).map_err(|error| {
            AgendarError::validation_at(
                "/runtime",
                "runtime_malformed",
                format!("runtime.json has an unexpected shape: {error}."),
            )
        })?;
        Ok(runtime)
    }

    /// The registered session for one harness identity in the same project.
    pub fn session_for_harness(
        &self,
        project: &ProjectSlug,
        harness: Harness,
        harness_session_id: &str,
    ) -> Option<(SessionId, &LocalSession)> {
        self.sessions
            .iter()
            .find(|(_, session)| {
                &session.project == project
                    && session.harness == harness
                    && session.harness_session_id == harness_session_id
                    && !session.lost
            })
            .map(|(id, session)| (id.clone(), session))
    }

    /// A registered, unsealed session.
    pub fn registered(&self, session: &SessionId) -> Option<&LocalSession> {
        self.sessions
            .get(session)
            .filter(|registration| !registration.lost && !registration.sealed)
    }

    /// Drop expired wait leases and old committed operation records so the
    /// bounded local journal cannot grow without limit. Reserved operations are
    /// always retained: they are the durable recovery material for an append.
    pub fn prune(&mut self, now: UtcTime) {
        self.wait_leases.retain(|_, lease| {
            lease
                .updated_at
                .add_seconds(lease.ttl_s.min(i64::MAX as u64) as i64)
                >= now
        });
        const MAX_COMMITTED_OPERATIONS: usize = 128;
        let cutoff = now.add_seconds(-7 * 24 * 60 * 60);
        let mut committed: Vec<(String, UtcTime)> = self
            .operations
            .iter()
            .filter(|(_, record)| record.status == OperationStatus::Committed)
            .map(|(key, record)| (key.clone(), record.updated_at))
            .collect();
        committed.sort_by(|left, right| right.1.cmp(&left.1));
        let mut removed = std::collections::BTreeSet::new();
        for (index, (key, updated_at)) in committed.into_iter().enumerate() {
            if index >= MAX_COMMITTED_OPERATIONS || updated_at < cutoff {
                removed.insert(key);
            }
        }
        for key in removed {
            self.operations.remove(&key);
        }
    }
}

/// Atomically mutate the runtime journal under its cross-process lock.
pub fn update_runtime<T>(
    local: &LocalPaths,
    vault: &Vault,
    action: impl FnOnce(&mut RuntimeState) -> Result<T>,
) -> Result<(T, u64)> {
    update_runtime_until(local, vault, None, action)
}

/// Atomically mutate the runtime journal, never waiting past `deadline`.
///
/// The bounded variant exists for a caller with an end-to-end operation budget:
/// waiting for the runtime lock must consume that budget, not run past it. A
/// caller that cannot take the lock before the deadline receives `E_BUSY` and
/// decides how to degrade (the Stop hook emits its pending fallback).
pub fn update_runtime_until<T>(
    local: &LocalPaths,
    vault: &Vault,
    deadline: Option<Instant>,
    action: impl FnOnce(&mut RuntimeState) -> Result<T>,
) -> Result<(T, u64)> {
    let layout = local.ensure_layout(vault.root(), vault.id())?;
    let _lock = acquire_lock_until(&layout.locks.join("runtime.lock"), deadline)?;
    let mut runtime = RuntimeState::load(local, vault)?;
    let value = action(&mut runtime)?;
    runtime.v = RUNTIME_VERSION;
    runtime.revision = runtime.revision.saturating_add(1);
    runtime.prune(SystemClock.now());
    write_json_atomic(&local.runtime_path(vault.id()), &runtime)?;
    Ok((value, runtime.revision))
}

/// Generate a canonical uppercase session ID (ULID).
pub fn new_session_id() -> Result<SessionId> {
    SessionId::parse(&ulid::Ulid::generate().to_string())
}

/// Generate a canonical uppercase event ID (ULID).
pub fn new_event_id() -> Result<EventId> {
    EventId::parse(&ulid::Ulid::generate().to_string())
}

/// OS process start identity in seconds since the Unix epoch, or `None` when the
/// process does not exist or cannot be observed.
///
/// `sysinfo` reports start time as seconds since boot; adding the boot time
/// turns it into a value comparable across processes and restarts.
pub fn process_identity(pid: u32) -> Option<u64> {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let process = system.process(Pid::from_u32(pid))?;
    Some(System::boot_time().saturating_add(process.start_time()))
}

/// Parent pid and executable/name basename of one process.
pub fn process_parent(pid: u32) -> Option<(u32, String)> {
    process_parent_with_script(pid).map(|(parent, name, _)| (parent, name))
}

/// Like [`process_parent`], plus the script a runtime (`node`, `bun`, `deno`)
/// runs, from the parent's second argument: npm-installed agents show up as
/// `node …/cli.js` on every platform.
fn process_parent_with_script(pid: u32) -> Option<(u32, String, Option<String>)> {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let process = system.process(Pid::from_u32(pid))?;
    let parent = process.parent()?;
    let parent_process = system.process(parent)?;
    let name = parent_process
        .exe()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .or_else(|| Some(parent_process.name().to_string_lossy().into_owned()))?;
    let script = parent_process
        .cmd()
        .get(1)
        .map(|argument| argument.to_string_lossy().replace('\\', "/"));
    Some((parent.as_u32(), name, script))
}

/// What identifies one harness's process: executable names (without `.exe`)
/// and, for agents run by a JavaScript runtime, a package path fragment.
fn harness_markers(
    harness: crate::model::Harness,
) -> (&'static [&'static str], &'static [&'static str]) {
    use crate::model::Harness;
    match harness {
        Harness::Codex => (&["codex"], &["@openai/codex"]),
        Harness::Pi => (&["pi"], &["pi-coding-agent"]),
        Harness::OpenCode => (&["opencode"], &["opencode-ai"]),
        // `agent` alone is too generic; the CLI runs node on its versioned bundle.
        Harness::Cursor => (&["cursor-agent"], &["cursor-agent/versions/"]),
        Harness::Dsh => (&["dsh"], &["@deepseek-ai/dsh", "deepseek-harness/"]),
        Harness::Agy => (&["agy"], &[]),
        Harness::Gemini => (&["gemini"], &["@google/gemini-cli"]),
        _ => (&["claude", "claude-code"], &["@anthropic-ai/claude-code"]),
    }
}

/// Whether a process named `name` (running `script`) is this harness. A
/// `.exe` suffix is ignored, and `<name>-<suffix>` counts (versioned builds).
pub fn is_harness_process(
    harness: crate::model::Harness,
    name: &str,
    script: Option<&str>,
) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    let (names, packages) = harness_markers(harness);
    if names
        .iter()
        .any(|expected| stem == *expected || stem.starts_with(&format!("{expected}-")))
    {
        return true;
    }
    let runtime = matches!(stem, "node" | "bun" | "deno");
    runtime && script.is_some_and(|script| packages.iter().any(|package| script.contains(package)))
}

/// Find the harness process in this process's parent ancestry.
///
/// At most eight levels are inspected. A configured executable name comes from
/// `AGENDAR_HARNESS_EXE` when set, otherwise the Claude Code names are accepted.
/// An ambiguous launcher returns `None` so the caller asks for an explicit
/// `--pid` instead of guessing.
pub fn find_harness_process() -> Option<(u32, u64)> {
    find_harness_process_for(crate::model::Harness::ClaudeCode)
}

/// The nearest ancestor process of this harness (see [`is_harness_process`]),
/// or the executable named by `AGENDAR_HARNESS_EXE`.
pub fn find_harness_process_for(harness: crate::model::Harness) -> Option<(u32, u64)> {
    let configured = std::env::var("AGENDAR_HARNESS_EXE").ok();
    let mut pid = std::process::id();
    for _ in 0..8 {
        let (parent, name, script) = process_parent_with_script(pid)?;
        let matches = match configured.as_deref() {
            Some(expected) => name == expected,
            None => is_harness_process(harness, &name, script.as_deref()),
        };
        if matches {
            return process_identity(parent).map(|start| (parent, start));
        }
        pid = parent;
    }
    None
}

#[cfg(test)]
mod harness_process_tests {
    use super::is_harness_process;
    use crate::model::Harness;

    #[test]
    fn harness_processes_match_on_every_platform() {
        assert!(is_harness_process(Harness::ClaudeCode, "claude", None));
        assert!(
            is_harness_process(Harness::ClaudeCode, "claude.exe", None),
            "Windows"
        );
        assert!(is_harness_process(Harness::ClaudeCode, "Claude.EXE", None));
        assert!(
            is_harness_process(
                Harness::ClaudeCode,
                "node",
                Some("/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js")
            ),
            "npm install on Linux"
        );
        assert!(
            is_harness_process(
                Harness::ClaudeCode,
                "node.exe",
                Some(
                    "C:/Users/a/AppData/Roaming/npm/node_modules/@anthropic-ai/claude-code/cli.js"
                )
            ),
            "npm install on Windows"
        );
        assert!(!is_harness_process(
            Harness::ClaudeCode,
            "node",
            Some("/srv/app.js")
        ));
        assert!(is_harness_process(Harness::Codex, "codex.exe", None));
        assert!(!is_harness_process(Harness::Codex, "claude", None));
        assert!(is_harness_process(
            Harness::Pi,
            "node",
            Some(
                "/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js"
            )
        ));
    }
}
