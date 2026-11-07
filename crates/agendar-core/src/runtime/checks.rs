//! Human-approved, bounded local check execution.
//!
//! A check profile is approved by the human for this host only. Approval binds
//! the exact argv, working directory and timeout to a digest; running re-hashes
//! the stored profile, so changing any part of it requires a new approval. The
//! command is executed directly (no shell), with a minimal environment, a fixed
//! cwd inside a linked repository, a bounded timeout and bounded output files
//! outside the Vault.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::cards::CheckResult;
use crate::model::ids::{CriterionId, EventId, Sha256, UtcTime};
use crate::model::state::canonical_json;
use crate::runtime::LocalPaths;
use crate::runtime::local_state::Config;
use crate::runtime::sessions::{RuntimeState, update_runtime};
use crate::vault::{Clock, SystemClock, Vault};

/// Largest output retained from one check (stdout+stderr combined).
pub const MAX_CHECK_OUTPUT_BYTES: usize = 262_144;
/// Default profile timeout in seconds.
pub const DEFAULT_CHECK_TIMEOUT_S: u32 = 30;
/// Largest accepted profile timeout in seconds.
pub const MAX_CHECK_TIMEOUT_S: u32 = 120;

/// One proposed check profile as the delivery or the UI supplies it.
///
/// The UI contract carries an opaque object; accept both the compact
/// `{argv,cwd,timeout_s}` shape and the native-boundary
/// `{executable,args,repo_root,timeout_s}` shape. Unknown extra fields are
/// ignored rather than trusted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criterion: Option<CriterionId>,
}

/// A profile after the two accepted input shapes have been collapsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedProfile {
    pub argv: Vec<String>,
    pub cwd: String,
    pub timeout_s: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criterion: Option<CriterionId>,
}

impl CheckProfile {
    pub fn normalize(&self) -> Result<NormalizedProfile> {
        let mut argv = if self.argv.is_empty() {
            let executable = self.executable.clone().ok_or_else(|| {
                AgendarError::validation_at(
                    "/profile/argv",
                    "check_argv_required",
                    "A check profile needs argv (or executable plus args).",
                )
            })?;
            let mut argv = vec![executable];
            argv.extend(self.args.iter().cloned());
            argv
        } else {
            self.argv.clone()
        };
        if argv.len() > 32 {
            return Err(AgendarError::validation_at(
                "/profile/argv",
                "check_argv_length",
                "A check profile may have at most 32 argv entries.",
            ));
        }
        if argv.is_empty() || argv[0].is_empty() {
            return Err(AgendarError::validation_at(
                "/profile/argv",
                "check_argv_required",
                "A check profile needs a non-empty executable.",
            ));
        }
        for argument in &argv {
            if argument.contains('\0') || argument.chars().any(char::is_control) {
                return Err(AgendarError::validation_at(
                    "/profile/argv",
                    "check_argv_control",
                    "A check argument must not contain NUL or control characters.",
                ));
            }
            if argument.len() > 4_096 {
                return Err(AgendarError::validation_at(
                    "/profile/argv",
                    "check_argv_length",
                    "A check argument must be at most 4096 bytes.",
                ));
            }
        }
        let cwd = self
            .cwd
            .clone()
            .or_else(|| self.repo_root.clone())
            .ok_or_else(|| {
                AgendarError::validation_at(
                    "/profile/cwd",
                    "check_cwd_required",
                    "A check profile needs a working directory.",
                )
            })?;
        let timeout_s = self.timeout_s.unwrap_or(DEFAULT_CHECK_TIMEOUT_S);
        if !(1..=MAX_CHECK_TIMEOUT_S).contains(&timeout_s) {
            return Err(AgendarError::validation_at(
                "/profile/timeout_s",
                "check_timeout",
                format!("A check timeout must be 1..={MAX_CHECK_TIMEOUT_S} seconds."),
            ));
        }
        argv.shrink_to_fit();
        Ok(NormalizedProfile {
            argv,
            cwd,
            timeout_s,
            delivery_id: self.delivery_id.clone(),
            criterion: self.criterion.clone(),
        })
    }
}

/// Stored approval: the exact normalized profile plus its binding digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckApproval {
    /// The binding digest, also used as the public `approval_id`.
    pub id: Sha256,
    pub profile: NormalizedProfile,
    /// Digest of `["check-profile", argv, cwd, timeout_s]`.
    pub revision: Sha256,
    pub approved_at: UtcTime,
}

/// Result of one approved check execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRun {
    pub result: CheckResult,
    pub output: Vec<u8>,
    pub started_at: UtcTime,
    pub finished_at: UtcTime,
    pub timed_out: bool,
    pub truncated: bool,
}

/// Digest binding one normalized profile. This function is public so tests and
/// the UI can reproduce the exact rule.
pub fn profile_digest(profile: &NormalizedProfile) -> Sha256 {
    let value = json!([
        "check-profile",
        profile.argv,
        profile.cwd,
        profile.timeout_s,
    ]);
    Sha256::from_bytes(canonical_json(&value).as_bytes())
}

/// Revision of "this profile is not approved yet", used by `approve_check`.
pub fn absent_revision(profile: &NormalizedProfile) -> Sha256 {
    let digest = profile_digest(profile);
    let value = json!(["check-approval", "absent", digest]);
    Sha256::from_bytes(canonical_json(&value).as_bytes())
}

/// Validate that `cwd` is a real directory inside one of this host's linked
/// repositories, optionally for one project.
pub fn validate_cwd(cwd: &str, config: &Config, project: Option<&str>) -> Result<PathBuf> {
    let path = PathBuf::from(cwd);
    let metadata = fs::metadata(&path).map_err(|error| {
        AgendarError::validation_at(
            "/profile/cwd",
            "check_cwd_missing",
            format!("The check working directory is not available: {error}."),
        )
    })?;
    if !metadata.is_dir() {
        return Err(AgendarError::validation_at(
            "/profile/cwd",
            "check_cwd_not_directory",
            "The check working directory must be a directory.",
        ));
    }
    let canonical = path.canonicalize()?;
    let mut best: Option<(usize, &str)> = None;
    for (root, mapped) in &config.repos {
        let root_path = PathBuf::from(root);
        if canonical.starts_with(&root_path) {
            let depth = root_path.components().count();
            if best.is_none_or(|(current, _)| depth > current) {
                best = Some((depth, mapped.as_str()));
            }
        }
    }
    let Some((_, mapped)) = best else {
        return Err(AgendarError::validation_at(
            "/profile/cwd",
            "check_cwd_outside_repo",
            "A check working directory must be inside a repository linked to a project.",
        )
        .with_hint("Link the repository first, then approve the check again."));
    };
    if let Some(project) = project
        && mapped != project
    {
        return Err(AgendarError::validation_at(
            "/profile/cwd",
            "check_project_mismatch",
            "The check working directory is linked to a different project.",
        ));
    }
    Ok(canonical)
}

/// Validate, normalize and persist one human approval. Repeating the same
/// profile returns the same id and writes nothing new.
pub fn approve(
    local: &LocalPaths,
    vault: &Vault,
    config: &Config,
    profile: &CheckProfile,
    approved_at: UtcTime,
) -> Result<CheckApproval> {
    let normalized = profile.normalize()?;
    validate_cwd(&normalized.cwd, config, None)?;
    let digest = profile_digest(&normalized);
    let approval = CheckApproval {
        id: digest.clone(),
        profile: normalized,
        revision: digest.clone(),
        approved_at,
    };
    update_runtime(local, vault, |runtime| {
        let key = approval_key(approval.id.as_str());
        if let Some(existing) = runtime.approvals.get(&key) {
            let stored: CheckApproval = serde_json::from_value(existing.clone()).map_err(|_| {
                AgendarError::new(ErrorCode::Io, "The stored check approval is unreadable.")
                    .with_detail("check_approval_corrupt")
            })?;
            if stored.profile == approval.profile {
                return Ok(approval.clone());
            }
        }
        runtime.approvals.insert(
            key,
            serde_json::to_value(&approval).map_err(|error| {
                AgendarError::new(
                    ErrorCode::Io,
                    format!("Cannot store the check approval: {error}."),
                )
                .with_detail("check_approval_store")
            })?,
        );
        Ok(approval.clone())
    })
    .map(|(approval, _)| approval)
}

/// Load one stored approval and verify its binding digest still matches.
pub fn load(local: &LocalPaths, vault: &Vault, approval_id: &str) -> Result<Option<CheckApproval>> {
    let runtime = RuntimeState::load(local, vault)?;
    let Some(value) = runtime.approvals.get(&approval_key(approval_id)) else {
        return Ok(None);
    };
    let approval: CheckApproval = serde_json::from_value(value.clone()).map_err(|_| {
        AgendarError::new(ErrorCode::Io, "The stored check approval is unreadable.")
            .with_detail("check_approval_corrupt")
    })?;
    let digest = profile_digest(&approval.profile);
    if digest != approval.id || digest != approval.revision {
        return Err(AgendarError::new(
            ErrorCode::StaleAnswer,
            "The approved check profile changed and must be approved again.",
        )
        .with_detail("check_profile_changed")
        .with_hint("Review the exact argv and approve it again."));
    }
    Ok(Some(approval))
}

fn approval_key(id: &str) -> String {
    format!("check:{id}")
}

/// Execute one already-approved profile. Never uses a shell; `argv` is passed
/// verbatim to the operating system.
pub fn run(local: &LocalPaths, vault: &Vault, approval: &CheckApproval) -> Result<CheckRun> {
    let layout = local.ensure_layout(vault.root(), vault.id())?;
    let staging = layout.tmp.join(format!("check-{}", ulid::Ulid::generate()));
    fs::create_dir_all(&staging)?;
    let stdout_path = staging.join("stdout");
    let stderr_path = staging.join("stderr");
    let stdout = File::create(&stdout_path)?;
    let stderr = File::create(&stderr_path)?;
    let started_at = SystemClock.now();

    let mut command = Command::new(&approval.profile.argv[0]);
    command
        .args(&approval.profile.argv[1..])
        .current_dir(&approval.profile.cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (key, value) in minimal_environment(&staging) {
        command.env(key, value);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(AgendarError::new(
                ErrorCode::Io,
                format!("Cannot start the approved check: {error}."),
            )
            .with_detail("check_spawn")
            .with_hint("Verify the approved executable still exists, then approve it again."));
        }
    };
    let deadline = Instant::now() + Duration::from_secs(u64::from(approval.profile.timeout_s));
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait()?;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_dir_all(&staging);
                return Err(AgendarError::from(error));
            }
        }
    };
    let finished_at = SystemClock.now();
    let (output, truncated) = read_bounded(&stdout_path, &stderr_path, MAX_CHECK_OUTPUT_BYTES)?;
    // The staging files are local garbage once their bounded bytes are in
    // memory; cleanup failure never turns a completed check into an error.
    let _ = fs::remove_dir_all(&staging);
    let result = if !timed_out && status.success() {
        CheckResult::Passed
    } else {
        CheckResult::Failed
    };
    Ok(CheckRun {
        result,
        output,
        started_at,
        finished_at,
        timed_out,
        truncated,
    })
}

/// Keep only platform runtime essentials: no tokens, API keys, harness
/// configuration or inherited user paths.
fn minimal_environment(staging: &Path) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::new();
    let path = if cfg!(windows) {
        "C:\\Windows\\System32;C:\\Windows".to_owned()
    } else {
        "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_owned()
    };
    environment.insert("PATH".to_owned(), path);
    environment.insert("LANG".to_owned(), "C".to_owned());
    environment.insert("LC_ALL".to_owned(), "C".to_owned());
    environment.insert("HOME".to_owned(), staging.to_string_lossy().into_owned());
    environment.insert(
        "USERPROFILE".to_owned(),
        staging.to_string_lossy().into_owned(),
    );
    environment.insert("TMPDIR".to_owned(), staging.to_string_lossy().into_owned());
    environment.insert("TEMP".to_owned(), staging.to_string_lossy().into_owned());
    environment.insert("TMP".to_owned(), staging.to_string_lossy().into_owned());
    if cfg!(windows) {
        environment.insert("SystemRoot".to_owned(), "C:\\Windows".to_owned());
        environment.insert("PATHEXT".to_owned(), ".COM;.EXE;.BAT;.CMD".to_owned());
    }
    environment
}

fn read_bounded(stdout: &Path, stderr: &Path, cap: usize) -> Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut truncated = false;
    let stdout_truncated = read_into(stdout, &mut output, cap)?;
    truncated |= stdout_truncated;
    if output.len() < cap {
        truncated |= read_into(stderr, &mut output, cap)?;
    } else if fs::metadata(stderr).is_ok_and(|metadata| metadata.len() > 0) {
        truncated = true;
    }
    Ok((output, truncated))
}

fn read_into(path: &Path, output: &mut Vec<u8>, cap: usize) -> Result<bool> {
    let remaining = cap.saturating_sub(output.len());
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let mut buffer = Vec::new();
    file.take(
        u64::try_from(remaining)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
    )
    .read_to_end(&mut buffer)?;
    if buffer.len() > remaining {
        buffer.truncate(remaining);
        output.extend_from_slice(&buffer);
        return Ok(true);
    }
    output.extend_from_slice(&buffer);
    Ok(false)
}

/// Persist a delivery's proposed check as local, non-authoritative metadata.
/// Approval is still a separate explicit human action.
pub fn store_proposal(
    local: &LocalPaths,
    vault: &Vault,
    delivery: &EventId,
    criterion: &CriterionId,
    profile: &NormalizedProfile,
    digest: &Sha256,
) -> Result<()> {
    let key = format!("proposal:{}:{}", delivery.as_str(), criterion.as_str());
    let value = json!({
        "kind": "check_proposal",
        "criterion": criterion,
        "profile": profile,
        "digest": digest,
    });
    update_runtime(local, vault, |runtime| {
        runtime.approvals.insert(key, value);
        Ok(())
    })
    .map(|_| ())
}

/// Project every stored proposal for this host as `<delivery id> -> rows`.
/// This is local metadata, never Vault truth.
pub fn proposals(local: &LocalPaths, vault: &Vault) -> Result<BTreeMap<String, Vec<Value>>> {
    let runtime = RuntimeState::load(local, vault)?;
    let mut rows: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for (key, value) in &runtime.approvals {
        let Some(rest) = key.strip_prefix("proposal:") else {
            continue;
        };
        let Some((delivery, _criterion)) = rest.split_once(':') else {
            continue;
        };
        let profile_value = match value.get("profile") {
            Some(profile) => profile,
            None => continue,
        };
        let Ok(profile) = serde_json::from_value::<NormalizedProfile>(profile_value.clone()) else {
            continue;
        };
        let Some(criterion) = value.get("criterion").cloned() else {
            continue;
        };
        let Some(digest) = value.get("digest").cloned() else {
            continue;
        };
        rows.entry(delivery.to_owned()).or_default().push(json!({
            "criterion": criterion,
            "argv": profile.argv,
            "cwd": profile.cwd,
            "timeout_s": profile.timeout_s,
            "digest": digest,
        }));
    }
    for values in rows.values_mut() {
        values.sort_by(|left, right| {
            let left_key = (
                left.get("criterion").and_then(Value::as_str),
                left.get("digest").and_then(Value::as_str),
            );
            let right_key = (
                right.get("criterion").and_then(Value::as_str),
                right.get("digest").and_then(Value::as_str),
            );
            left_key.cmp(&right_key)
        });
    }
    Ok(rows)
}

/// Best-effort JSON view of an approval (used by diagnostics/tests).
pub fn approval_json(approval: &CheckApproval) -> Value {
    serde_json::to_value(approval).unwrap_or(Value::Null)
}
