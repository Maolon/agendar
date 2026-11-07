//! Agents seen without the skill: a host-local record fed by observe-only
//! harness hooks (`agendar hook observe`). It says only whether an agent is
//! working or waiting at its prompt, where it runs and since when. Nothing here
//! is a question, nothing is written to the Vault, and the hook never prints a
//! word the model could read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::ids::UtcTime;
use crate::runtime::local_state::{acquire_lock_until, write_json_atomic};

/// Entries not updated for this long are forgotten.
pub const PRESENCE_TTL_MS: i64 = 12 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceState {
    Working,
    /// Stopped and waiting at its prompt for the human.
    Waiting,
    /// Asking the human for a permission.
    Permission,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub harness: String,
    pub cwd: PathBuf,
    pub state: PresenceState,
    /// When the current state began.
    pub since: UtcTime,
    pub updated_at: UtcTime,
    /// The harness process, to tell it apart from a skill session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceFile {
    /// Keyed by `<harness>:<harness session id>`.
    pub agents: BTreeMap<String, Presence>,
}

fn path(data_root: &Path) -> PathBuf {
    data_root.join("presence.json")
}

/// Read the record; a missing or unreadable file is empty.
pub fn load(data_root: &Path) -> PresenceFile {
    std::fs::read(path(data_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// What one hook call reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    State(PresenceState),
    /// The session ended: forget it.
    Ended,
}

/// Record one observation under the presence lock (bounded wait: a hook must
/// never hold its harness up).
pub fn record(
    data_root: &Path,
    key: &str,
    harness: &str,
    cwd: &Path,
    pid: Option<u32>,
    observation: Observation,
    now: UtcTime,
) -> Result<()> {
    std::fs::create_dir_all(data_root).map_err(|error| {
        crate::error::AgendarError::new(
            crate::error::ErrorCode::Io,
            format!("Cannot create the data folder: {error}."),
        )
    })?;
    let _lock = acquire_lock_until(
        &data_root.join("presence.lock"),
        Some(Instant::now() + Duration::from_secs(2)),
    )?;
    let mut file = load(data_root);
    file.agents
        .retain(|_, agent| now.millis() - agent.updated_at.millis() <= PRESENCE_TTL_MS);
    match observation {
        Observation::Ended => {
            file.agents.remove(key);
        }
        Observation::State(state) => {
            let since = file
                .agents
                .get(key)
                .filter(|known| known.state == state)
                .map_or(now, |known| known.since);
            file.agents.insert(
                key.to_owned(),
                Presence {
                    harness: harness.to_owned(),
                    cwd: cwd.to_path_buf(),
                    state,
                    since,
                    updated_at: now,
                    pid,
                },
            );
        }
    }
    write_json_atomic(&path(data_root), &file)
}

/// Map a Claude Code hook event (and its notification type) to an observation.
pub fn claude_observation(event: &str, notification: Option<&str>) -> Option<Observation> {
    match event {
        "UserPromptSubmit" | "PreToolUse" => Some(Observation::State(PresenceState::Working)),
        "Stop" => Some(Observation::State(PresenceState::Waiting)),
        "Notification" => Some(Observation::State(
            if notification == Some("permission_prompt") {
                PresenceState::Permission
            } else {
                PresenceState::Waiting
            },
        )),
        "SessionEnd" => Some(Observation::Ended),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: i64) -> UtcTime {
        UtcTime::parse("2026-11-06T10:00:00.000Z")
            .expect("time")
            .add_millis(minute * 60_000)
    }

    #[test]
    fn observations_keep_their_start_and_end_sessions() {
        let dir = tempfile::tempdir().expect("dir");
        let cwd = Path::new("/work/app");
        let working = Observation::State(PresenceState::Working);
        let waiting = Observation::State(PresenceState::Waiting);
        record(
            dir.path(),
            "claude:a",
            "claude_code",
            cwd,
            Some(7),
            working.clone(),
            at(0),
        )
        .expect("record");
        record(
            dir.path(),
            "claude:a",
            "claude_code",
            cwd,
            Some(7),
            working,
            at(5),
        )
        .expect("record");
        let file = load(dir.path());
        assert_eq!(
            file.agents["claude:a"].since,
            at(0),
            "the same state keeps its start"
        );
        record(
            dir.path(),
            "claude:a",
            "claude_code",
            cwd,
            Some(7),
            waiting,
            at(9),
        )
        .expect("record");
        let file = load(dir.path());
        assert_eq!(file.agents["claude:a"].state, PresenceState::Waiting);
        assert_eq!(file.agents["claude:a"].since, at(9));
        record(
            dir.path(),
            "claude:a",
            "claude_code",
            cwd,
            None,
            Observation::Ended,
            at(10),
        )
        .expect("end");
        assert!(load(dir.path()).agents.is_empty());
        assert_eq!(
            claude_observation("Notification", Some("permission_prompt")),
            Some(Observation::State(PresenceState::Permission))
        );
        assert_eq!(claude_observation("PostToolUse", None), None);
    }
}
