//! Local installation plans: the Claude Code hook installer and the bundled CLI
//! PATH link.
//!
//! These are local, consented side effects outside the Vault. Every write is planned first, the plan carries a digest, and the write
//! re-reads the destination and refuses a changed plan. Unrelated settings,
//! hook entries and matchers are preserved; a target that is a symlink (or has a
//! symlinked ancestor) is refused instead of followed.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::{Harness, Sha256, canonical_json};
use crate::service::shell_quote;

/// Maximum settings file we will read before refusing to touch it.
pub const MAX_SETTINGS_BYTES: u64 = 1_048_576;

/// One installed hook command in a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookChange {
    pub event: String,
    pub command: String,
    pub timeout_s: u32,
    /// Claude Code `asyncRewake`: runs in the background and wakes an idle
    /// session when it exits 2.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub async_rewake: bool,
}

/// What an installation would do (or did).
///
/// This is the shared `InstallPlan` shape plus the fields the
/// setup screen needs: the exact PATH command and the plan digest that a
/// non-dry-run call must echo back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallPlan {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<Harness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_path: Option<String>,
    pub changes: Vec<HookChange>,
    pub requires_consent: bool,
    pub plan_digest: Sha256,
    pub already_installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_path: Option<String>,
}

impl InstallPlan {
    /// Recompute the digest from the plan's stable content.
    fn seal(mut self) -> Result<Self> {
        let mut value = serde_json::to_value(&self).map_err(|error| {
            AgendarError::new(
                ErrorCode::Io,
                format!("Cannot serialize the installation plan: {error}."),
            )
            .with_detail("install_plan_serialize")
        })?;
        if let Value::Object(map) = &mut value {
            map.remove("plan_digest");
        }
        self.plan_digest = Sha256::from_bytes(canonical_json(&value).as_bytes());
        Ok(self)
    }
}

/// Which hooks an installation writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookSet {
    /// The skill protocol's hooks: context and Stop waits.
    Skill,
    /// Observe-only hooks for agents without the skill: they record working or
    /// waiting locally and never print anything the model reads.
    Observe,
    /// The skill protocol's hooks for Codex (`~/.codex/hooks.json`): the same
    /// reminders and hold-once Stop; Codex has no rewake, its queue wakes it.
    Codex,
}

impl HookSet {
    /// The harness whose hook file this set is written to.
    pub fn harness(self) -> Harness {
        match self {
            Self::Codex => Harness::Codex,
            Self::Skill | Self::Observe => Harness::ClaudeCode,
        }
    }
}

fn commands_for(set: HookSet, cli: &Path) -> Result<Vec<HookChange>> {
    match set {
        HookSet::Skill => hook_commands(cli),
        HookSet::Codex => {
            let quoted = shell_quote(&cli.to_string_lossy());
            Ok([
                ("SessionStart", "session-start", 60),
                ("UserPromptSubmit", "user-prompt-submit", 10),
                ("PostToolUse", "post-tool-use", 10),
                ("Stop", "stop", 60),
            ]
            .into_iter()
            .map(|(event, action, timeout_s)| HookChange {
                event: event.to_owned(),
                command: format!("{quoted} hook --harness codex {action}"),
                timeout_s,
                async_rewake: false,
            })
            .collect())
        }
        HookSet::Observe => {
            let quoted = shell_quote(&cli.to_string_lossy());
            Ok(["UserPromptSubmit", "Stop", "Notification", "SessionEnd"]
                .into_iter()
                .map(|event| HookChange {
                    event: event.to_owned(),
                    command: format!("{quoted} hook observe"),
                    timeout_s: 5,
                    async_rewake: false,
                })
                .collect())
        }
    }
}

/// The exact hook configuration
fn hook_commands(cli: &Path) -> Result<Vec<HookChange>> {
    let quoted = shell_quote(&cli.to_string_lossy());
    Ok(vec![
        HookChange {
            event: "SessionStart".to_owned(),
            command: format!("{quoted} hook session-start"),
            timeout_s: 60,
            async_rewake: false,
        },
        HookChange {
            event: "UserPromptSubmit".to_owned(),
            command: format!("{quoted} hook user-prompt-submit"),
            timeout_s: 10,
            async_rewake: false,
        },
        HookChange {
            event: "PostToolUse".to_owned(),
            command: format!("{quoted} hook post-tool-use"),
            timeout_s: 10,
            async_rewake: false,
        },
        HookChange {
            event: "Stop".to_owned(),
            command: format!("{quoted} hook stop"),
            timeout_s: 60,
            async_rewake: false,
        },
        // An answer that arrives after the turn ended wakes the idle session.
        HookChange {
            event: "Stop".to_owned(),
            command: format!("{quoted} hook rewake"),
            timeout_s: 3600,
            async_rewake: true,
        },
    ])
}

/// The user's home from the environment: `HOME`, else `USERPROFILE` (Windows
/// rarely sets HOME), or a typed context error. Never an OS-resolved real-user
/// home, so a redirected HOME in a test never touches the operator's settings.
pub fn home_dir() -> Result<PathBuf> {
    home_from(std::env::var_os("HOME"), std::env::var_os("USERPROFILE"))
}

/// [`home_dir`] over explicit values (testable without touching the process
/// environment).
pub fn home_from(
    home: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    home.filter(|value| !value.is_empty())
        .or_else(|| userprofile.filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .ok_or_else(|| {
            AgendarError::new(
                ErrorCode::Context,
                "Neither HOME nor USERPROFILE is set, so the agent settings files cannot be resolved.",
            )
            .with_detail("missing_home")
            .with_hint("Set HOME (USERPROFILE on Windows) or pass an explicit --settings path.")
        })
}

/// Claude Code's configuration folder: `CLAUDE_CONFIG_DIR` when set, else
/// `<home>/.claude` (the same on macOS, Linux and Windows).
pub fn claude_dir(home: &Path) -> PathBuf {
    env_dir("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude"))
}

/// Codex's home: `CODEX_HOME` when set, else `<home>/.codex`.
pub fn codex_dir(home: &Path) -> PathBuf {
    env_dir("CODEX_HOME").unwrap_or_else(|| home.join(".codex"))
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Resolve the settings file for this install, honouring an explicit override.
pub fn claude_settings_path(home: &Path, settings: Option<&Path>) -> PathBuf {
    match settings {
        Some(path) => path.to_path_buf(),
        None => claude_dir(home).join("settings.json"),
    }
}

/// The file a hook set is written to: Claude Code's settings, or Codex's
/// user-level `hooks.json` (same `{"hooks": {...}}` shape).
pub fn hook_file_path(set: HookSet, home: &Path, settings: Option<&Path>) -> PathBuf {
    match (set, settings) {
        (_, Some(path)) => path.to_path_buf(),
        (HookSet::Codex, None) => codex_dir(home).join("hooks.json"),
        (_, None) => claude_settings_path(home, None),
    }
}

/// Refuse a target that is a symlink, or a symlinked component below the
/// user's home directory.
///
/// The walk is anchored at `$HOME`, not at the filesystem root: platform
/// aliases such as macOS `/var` are ordinary system symlinks and refusing them
/// would make every temporary path unusable without protecting the user's own
/// files. An explicit settings path outside HOME only refuses itself and its
/// immediate parent.
fn refuse_symlinked_target(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(symlink_refusal(path));
    }
    let home = home_dir().ok();
    let anchored = home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok().map(|rest| (home, rest)));
    if let Some((home, rest)) = anchored {
        let mut current = home.to_path_buf();
        for component in rest.components() {
            current.push(component);
            if current == path {
                break;
            }
            if fs::symlink_metadata(&current)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
            {
                return Err(symlink_refusal(&current));
            }
        }
        return Ok(());
    }
    if let Some(parent) = path.parent()
        && fs::symlink_metadata(parent)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
    {
        return Err(symlink_refusal(parent));
    }
    Ok(())
}

fn symlink_refusal(path: &Path) -> AgendarError {
    AgendarError::new(
        ErrorCode::Unsupported,
        format!("Refusing to follow a symlink at {}.", path.display()),
    )
    .with_detail("symlink_target")
    .with_hint("Point the installer at a real settings path.")
}

fn read_settings(path: &Path) -> Result<Value> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({}));
        }
        Err(error) => return Err(Into::into(error)),
    };
    if metadata.file_type().is_symlink() {
        return Err(AgendarError::new(
            ErrorCode::Unsupported,
            "Refusing to edit a symlinked settings file.",
        )
        .with_detail("symlink_target"));
    }
    if !metadata.is_file() {
        return Err(AgendarError::validation_at(
            "/settings",
            "not_a_file",
            "The agent config path is not a regular file.",
        ));
    }
    if metadata.len() > MAX_SETTINGS_BYTES {
        return Err(AgendarError::validation_at(
            "/settings",
            "settings_too_large",
            "The agent config file is larger than 1 MiB.",
        ));
    }
    let bytes = fs::read(path)?;
    // An empty file (some agents create their config empty) holds no settings.
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        AgendarError::validation_at(
            "/settings",
            "settings_malformed",
            format!("The existing settings file is not valid JSON: {error}."),
        )
        .with_hint("Fix or remove the file; nothing was changed.")
    })?;
    if !value.is_object() {
        return Err(AgendarError::validation_at(
            "/settings",
            "settings_not_an_object",
            "The settings file must contain one JSON object.",
        ));
    }
    Ok(value)
}

fn has_command(entries: &[Value], command: &str) -> bool {
    entries.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("type").and_then(Value::as_str) == Some("command")
                        && hook.get("command").and_then(Value::as_str) == Some(command)
                })
            })
    })
}

fn event_entries<'a>(hooks: &'a Map<String, Value>, event: &str) -> Result<&'a Vec<Value>> {
    match hooks.get(event) {
        Some(Value::Array(entries)) => Ok(entries),
        Some(_) => Err(AgendarError::validation_at(
            format!("/hooks/{event}"),
            "hook_event_malformed",
            format!("The existing {event} hook value is not an array."),
        )),
        None => Ok(&EMPTY_ENTRIES),
    }
}

static EMPTY_ENTRIES: Vec<Value> = Vec::new();

fn planned_value(current: &Value, changes: &[HookChange]) -> Result<(Value, Vec<HookChange>)> {
    let mut value = current.clone();
    let object = value.as_object_mut().ok_or_else(|| {
        AgendarError::validation_at(
            "/settings",
            "settings_not_an_object",
            "The settings file must contain one JSON object.",
        )
    })?;
    let hooks_value = object
        .entry("hooks".to_owned())
        .or_insert_with(|| json!({}));
    let hooks = hooks_value.as_object_mut().ok_or_else(|| {
        AgendarError::validation_at(
            "/hooks",
            "hooks_not_an_object",
            "The settings `hooks` value must be an object.",
        )
    })?;
    let mut missing = Vec::new();
    for change in changes {
        let entries = event_entries(hooks, &change.event)?;
        if has_command(entries, &change.command) {
            continue;
        }
        missing.push(change.clone());
        let mut hook = json!({
            "type": "command",
            "command": change.command,
            "timeout": change.timeout_s,
        });
        if change.async_rewake {
            hook["asyncRewake"] = json!(true);
        }
        let entry = if change.event == "PostToolUse" {
            json!({"matcher": ".*", "hooks": [hook]})
        } else {
            json!({"hooks": [hook]})
        };
        hooks
            .entry(change.event.clone())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("event entry checked above")
            .push(entry);
    }
    Ok((value, missing))
}

/// Plan the Claude Code hook installation for one CLI executable.
pub fn plan_claude_hooks(cli: &Path, settings: Option<&Path>, home: &Path) -> Result<InstallPlan> {
    plan_claude_hook_set(HookSet::Skill, cli, settings, home)
}

/// Plan one hook set into Claude Code's settings (dry run, nothing written).
pub fn plan_claude_hook_set(
    set: HookSet,
    cli: &Path,
    settings: Option<&Path>,
    home: &Path,
) -> Result<InstallPlan> {
    let settings_path = hook_file_path(set, home, settings);
    refuse_symlinked_target(&settings_path)?;
    let current = read_settings(&settings_path)?;
    let changes = commands_for(set, cli)?;
    let (_, missing) = planned_value(&current, &changes)?;
    InstallPlan {
        harness: Some(set.harness()),
        settings_path: Some(settings_path.to_string_lossy().into_owned()),
        changes: missing.clone(),
        requires_consent: true,
        plan_digest: Sha256::from_bytes(b""),
        already_installed: missing.is_empty(),
        destination: None,
        command: None,
        cli_path: Some(cli.to_string_lossy().into_owned()),
    }
    .seal()
}

/// Where one harness keeps its MCP servers, and the `agendar` entry for it.
struct McpTarget {
    path: PathBuf,
    /// The object that holds server entries (`mcpServers` or opencode's `mcp`).
    servers_key: &'static str,
    entry: Value,
}

fn mcp_target(harness: Harness, cli: &Path, home: &Path) -> Result<McpTarget> {
    let cli = cli.to_string_lossy().into_owned();
    let standard = |path: PathBuf, name: &str| McpTarget {
        path,
        servers_key: "mcpServers",
        entry: json!({"command": cli, "args": ["mcp", "--harness", name]}),
    };
    let config_home = env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
    match harness {
        Harness::Pi => Ok(standard(
            env_dir("PI_CODING_AGENT_DIR")
                .unwrap_or_else(|| home.join(".pi").join("agent"))
                .join("mcp.json"),
            "pi",
        )),
        Harness::Cursor => Ok(standard(home.join(".cursor").join("mcp.json"), "cursor")),
        Harness::Agy => Ok(standard(
            home.join(".gemini").join("config").join("mcp_config.json"),
            "agy",
        )),
        Harness::Gemini => Ok(standard(
            env_dir("GEMINI_CLI_HOME")
                .unwrap_or_else(|| home.to_path_buf())
                .join(".gemini")
                .join("settings.json"),
            "gemini",
        )),
        Harness::OpenCode => Ok(McpTarget {
            path: config_home.join("opencode").join("opencode.json"),
            servers_key: "mcp",
            entry: json!({
                "type": "local",
                "command": [cli, "mcp", "--harness", "opencode"],
                "enabled": true,
            }),
        }),
        Harness::Dsh => Err(AgendarError::new(
            ErrorCode::Unsupported,
            "dsh keeps MCP servers in its Cordis YAML config; add Agendar there by hand.",
        )
        .with_detail("mcp_manual")
        .with_hint(format!(
            "Add an MCP server row to dsh's mcp-client config: transport stdio, command `{cli}`, args `mcp --harness dsh`, then restart dsh."
        ))),
        _ => Err(AgendarError::new(
            ErrorCode::Unsupported,
            "This harness uses Agendar's hooks (install-hooks) or the CLI with the skill.",
        )
        .with_detail("mcp_harness_unsupported")
        .with_hint("Use --harness pi, opencode, cursor, agy or gemini.")),
    }
}

/// Plan adding the `agendar` MCP server to one harness's config (dry run).
/// The file is merged, never replaced; a file that is not plain JSON (for
/// example JSON with comments) is refused untouched. Keys are rewritten in
/// sorted order, and the first write keeps one backup.
pub fn plan_mcp(harness: Harness, cli: &Path, home: &Path) -> Result<InstallPlan> {
    let target = mcp_target(harness, cli, home)?;
    refuse_symlinked_target(&target.path)?;
    let current = read_settings(&target.path)?;
    let installed = current
        .get(target.servers_key)
        .and_then(|servers| servers.get("agendar"))
        == Some(&target.entry);
    InstallPlan {
        harness: Some(harness),
        settings_path: Some(target.path.to_string_lossy().into_owned()),
        changes: Vec::new(),
        requires_consent: true,
        plan_digest: Sha256::from_bytes(b""),
        already_installed: installed,
        destination: None,
        command: Some(canonical_json(&target.entry)),
        cli_path: Some(cli.to_string_lossy().into_owned()),
    }
    .seal()
}

/// Apply a planned MCP install after explicit consent (same guarantees as the
/// hook installer: re-planned, stale plan refused, one backup).
pub fn install_mcp(
    harness: Harness,
    cli: &Path,
    home: &Path,
    expected_plan_digest: &Sha256,
) -> Result<InstallPlan> {
    let plan = plan_mcp(harness, cli, home)?;
    if &plan.plan_digest != expected_plan_digest {
        return Err(AgendarError::new(
            ErrorCode::StaleAnswer,
            "The MCP config changed after the plan was shown.",
        )
        .with_detail("install_plan_stale")
        .with_hint("Review the new plan and consent again."));
    }
    if plan.already_installed {
        return Ok(plan);
    }
    let target = mcp_target(harness, cli, home)?;
    let mut value = read_settings(&target.path)?;
    let root = value.as_object_mut().ok_or_else(|| {
        AgendarError::validation_at(
            "/settings",
            "settings_not_object",
            "The MCP config is not a JSON object.",
        )
    })?;
    let servers = root
        .entry(target.servers_key.to_owned())
        .or_insert_with(|| json!({}));
    let servers = servers.as_object_mut().ok_or_else(|| {
        AgendarError::validation_at(
            "/settings",
            "mcp_servers_not_object",
            format!(
                "`{}` in the MCP config is not an object.",
                target.servers_key
            ),
        )
    })?;
    servers.insert("agendar".to_owned(), target.entry.clone());
    if let Some(parent) = target.path.parent() {
        refuse_symlinked_target(parent)?;
        fs::create_dir_all(parent)?;
    }
    if target.path.exists() {
        backup_once(&target.path)?;
    }
    let bytes = serde_json::to_vec_pretty(&value).map_err(|error| {
        AgendarError::new(
            ErrorCode::Io,
            format!("Cannot serialize the MCP config: {error}."),
        )
        .with_detail("settings_serialize")
    })?;
    write_atomic(&target.path, &bytes)?;
    Ok(plan)
}

/// Apply a previously planned hook installation after explicit consent.
///
/// The destination is re-read and re-planned; a changed plan is
/// `E_STALE_ANSWER` and writes nothing. Exactly one adjacent backup is created
/// per change, and an already installed file is not rewritten.
pub fn install_claude_hooks(
    cli: &Path,
    settings: Option<&Path>,
    home: &Path,
    expected_plan_digest: &Sha256,
) -> Result<InstallPlan> {
    install_claude_hook_set(HookSet::Skill, cli, settings, home, expected_plan_digest)
}

/// Apply one planned hook set after explicit consent (see
/// [`install_claude_hooks`] for the guarantees).
pub fn install_claude_hook_set(
    set: HookSet,
    cli: &Path,
    settings: Option<&Path>,
    home: &Path,
    expected_plan_digest: &Sha256,
) -> Result<InstallPlan> {
    let plan = plan_claude_hook_set(set, cli, settings, home)?;
    if &plan.plan_digest != expected_plan_digest {
        return Err(AgendarError::new(
            ErrorCode::StaleAnswer,
            "The hook settings changed after the plan was shown.",
        )
        .with_detail("install_plan_stale")
        .with_hint("Review the new plan and consent again."));
    }
    if plan.already_installed {
        return Ok(plan);
    }
    let settings_path = hook_file_path(set, home, settings);
    let current = read_settings(&settings_path)?;
    let commands = commands_for(set, cli)?;
    let (value, missing) = planned_value(&current, &commands)?;
    if missing.is_empty() {
        return Ok(plan);
    }
    if let Some(parent) = settings_path.parent() {
        refuse_symlinked_target(parent)?;
        fs::create_dir_all(parent)?;
    }
    if settings_path.exists() {
        backup_once(&settings_path)?;
    }
    let bytes = serde_json::to_vec_pretty(&value).map_err(|error| {
        AgendarError::new(
            ErrorCode::Io,
            format!("Cannot serialize the settings file: {error}."),
        )
        .with_detail("settings_serialize")
    })?;
    write_atomic(&settings_path, &bytes)?;
    Ok(plan)
}

fn backup_once(path: &Path) -> Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    for attempt in 0..1000u32 {
        let suffix = if attempt == 0 {
            format!("agendar-backup-{stamp}")
        } else {
            format!("agendar-backup-{stamp}-{attempt}")
        };
        let candidate = path.with_file_name(format!(
            "{}.{}",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "settings.json".to_owned()),
            suffix
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => {
                fs::copy(path, &candidate)?;
                if let Ok(metadata) = fs::metadata(path) {
                    let _ = fs::set_permissions(&candidate, metadata.permissions());
                }
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(
        AgendarError::new(ErrorCode::Io, "Cannot create a unique settings backup.")
            .with_detail("settings_backup"),
    )
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        AgendarError::validation_at(
            "/settings",
            "settings_parent",
            "The settings path has no parent directory.",
        )
    })?;
    // The staging leaf is owned by this call: it is created exclusively with an
    // unpredictable name, so a pre-existing file or symbolic link can never be
    // followed, truncated or renamed over the destination.
    let (file, temp) = create_staging_file(parent)?;
    let staged = stage_and_rename(path, parent, &temp, file, bytes);
    if staged.is_err() {
        let _ = fs::remove_file(&temp);
    }
    staged
}

/// Write the staged bytes, sync them and rename the owned leaf into place.
fn stage_and_rename(
    path: &Path,
    parent: &Path,
    temp: &Path,
    mut file: fs::File,
    bytes: &[u8],
) -> Result<()> {
    use std::io::Write as _;

    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(path)
            .map(|metadata| metadata.permissions().mode())
            .unwrap_or(0o600);
        fs::set_permissions(temp, fs::Permissions::from_mode(mode))?;
    }
    fs::rename(temp, path)?;
    crate::runtime::sync_dir(parent)?;
    Ok(())
}

/// Create one fresh staging file for the atomic settings write.
///
/// The name carries a random component and the open is `create_new`, so a leaf
/// that already exists — including a dangling symbolic link — is refused rather
/// than opened. The file is owner-only from creation, before any settings
/// content is written.
fn create_staging_file(parent: &Path) -> Result<(fs::File, PathBuf)> {
    for _ in 0..8 {
        let candidate = parent.join(format!(
            ".agendar-settings-{}-{}.tmp",
            std::process::id(),
            ulid::Ulid::generate()
        ));
        match open_new_leaf(&candidate) {
            Ok(file) => return Ok((file, candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(AgendarError::new(
        ErrorCode::Io,
        "Cannot create a private settings staging file.",
    )
    .with_detail("settings_staging"))
}

/// Open a brand-new owner-only leaf, refusing any pre-existing path.
///
/// `create_new` makes the call fail with `AlreadyExists` for an existing file,
/// directory or symbolic link (even a dangling one), so the caller never opens,
/// truncates or renames an object it did not create. On Unix the permissions
/// are owner-only from the moment the leaf exists.
fn open_new_leaf(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// Plan the PATH link for the bundled CLI.
pub fn plan_path_link(cli: &Path, destination: Option<&Path>, home: &Path) -> Result<InstallPlan> {
    let destination = destination
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".local").join("bin").join(cli_name()));
    let installed = path_link_matches(cli, &destination)?;
    if !installed {
        refuse_symlinked_target(&destination)?;
    }
    let command = if cfg!(windows) {
        format!(
            "copy /Y \"{}\" \"{}\"",
            cli.display(),
            destination.display()
        )
    } else {
        format!(
            "ln -s {} {}",
            shell_quote(&cli.to_string_lossy()),
            shell_quote(&destination.to_string_lossy())
        )
    };
    InstallPlan {
        harness: None,
        settings_path: None,
        changes: Vec::new(),
        requires_consent: true,
        plan_digest: Sha256::from_bytes(b""),
        already_installed: installed,
        destination: Some(destination.to_string_lossy().into_owned()),
        command: Some(command),
        cli_path: Some(cli.to_string_lossy().into_owned()),
    }
    .seal()
}

fn cli_name() -> &'static str {
    if cfg!(windows) {
        "agendar.exe"
    } else {
        "agendar"
    }
}

fn path_link_matches(cli: &Path, destination: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(destination) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(destination)?;
        let resolved = if target.is_absolute() {
            target
        } else {
            destination
                .parent()
                .map(|parent| parent.join(&target))
                .unwrap_or(target)
        };
        return Ok(resolved.canonicalize().ok() == cli.canonicalize().ok());
    }
    Ok(false)
}

/// Apply the PATH link after consent.
pub fn install_path_link(
    cli: &Path,
    destination: Option<&Path>,
    home: &Path,
    expected_plan_digest: &Sha256,
) -> Result<InstallPlan> {
    let plan = plan_path_link(cli, destination, home)?;
    if &plan.plan_digest != expected_plan_digest {
        return Err(AgendarError::new(
            ErrorCode::StaleAnswer,
            "The PATH destination changed after the plan was shown.",
        )
        .with_detail("install_plan_stale")
        .with_hint("Review the new plan and consent again."));
    }
    if plan.already_installed {
        return Ok(plan);
    }
    let destination = PathBuf::from(
        plan.destination
            .as_deref()
            .expect("a path-link plan always names a destination"),
    );
    refuse_symlinked_target(&destination)?;
    if let Ok(metadata) = fs::symlink_metadata(&destination) {
        if metadata.file_type().is_symlink() {
            return Err(AgendarError::new(
                ErrorCode::Unsupported,
                "Refusing to overwrite a symlink that points somewhere else.",
            )
            .with_detail("path_link_conflict"));
        }
        return Err(AgendarError::new(
            ErrorCode::Unsupported,
            "Refusing to overwrite an unrelated file in the PATH directory.",
        )
        .with_detail("path_link_conflict"));
    }
    if let Some(parent) = destination.parent() {
        refuse_symlinked_target(parent)?;
        fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(cli, &destination)?;
    }
    #[cfg(not(unix))]
    {
        fs::copy(cli, &destination)?;
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_home_is_a_context_error() {
        // The helper reads the process environment, so this test only checks
        // the explicit path resolver and a non-empty home.
        let home = Path::new("/tmp/agendar-home");
        assert_eq!(
            claude_settings_path(home, None),
            Path::new("/tmp/agendar-home/.claude/settings.json")
        );
        let override_path = Path::new("/tmp/custom.json");
        assert_eq!(
            claude_settings_path(home, Some(override_path)),
            override_path
        );
    }

    #[test]
    fn the_observe_set_adds_only_its_silent_hooks_beside_existing_ones() {
        let home = tempfile::tempdir().expect("home");
        let settings = home.path().join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().expect("dir")).expect("dir");
        std::fs::write(
            &settings,
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"my-own-stop"}]}]}}"#,
        )
        .expect("settings");
        let cli = Path::new("/opt/agendar/bin/agendar");
        let plan = plan_claude_hook_set(HookSet::Observe, cli, None, home.path()).expect("plan");
        let events: Vec<&str> = plan
            .changes
            .iter()
            .map(|change| change.event.as_str())
            .collect();
        assert_eq!(
            events,
            ["UserPromptSubmit", "Stop", "Notification", "SessionEnd"]
        );
        assert!(
            plan.changes
                .iter()
                .all(|change| change.command.ends_with("hook observe"))
        );
        let done =
            install_claude_hook_set(HookSet::Observe, cli, None, home.path(), &plan.plan_digest)
                .expect("install");
        assert!(!done.already_installed);
        let written = std::fs::read_to_string(&settings).expect("written");
        assert!(written.contains("my-own-stop"), "the user's own hook stays");
        assert_eq!(written.matches("hook observe").count(), 4);
        assert!(
            plan_claude_hook_set(HookSet::Observe, cli, None, home.path())
                .expect("again")
                .already_installed
        );
    }

    // -----------------------------------------------------------------
    // Staging leaf safety (1fh review B2)
    // -----------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn a_preexisting_staging_symlink_is_never_followed() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("dir");
        let victim = dir.path().join("victim.json");
        fs::write(&victim, br#"{"permissions":{"allow":["Bash(ls)"]}}"#).expect("victim");
        let settings = dir.path().join("settings.json");
        // The old - predictable - staging name from the review's repro.
        let planted = dir
            .path()
            .join(format!(".agendar-settings-{}.tmp", std::process::id()));
        symlink(&victim, &planted).expect("planted symlink");

        write_atomic(&settings, br#"{"hooks":{}}"#).expect("atomic write");

        assert_eq!(
            fs::read(&victim).expect("victim bytes"),
            br#"{"permissions":{"allow":["Bash(ls)"]}}"#,
            "the victim behind a staging symlink must never be written"
        );
        let metadata = fs::symlink_metadata(&settings).expect("settings metadata");
        assert!(
            metadata.file_type().is_file(),
            "the settings leaf must be a regular file, not a symlink"
        );
        assert_eq!(
            fs::read(&settings).expect("settings bytes"),
            br#"{"hooks":{}}"#
        );
        assert!(
            fs::symlink_metadata(&planted)
                .expect("planted metadata")
                .file_type()
                .is_symlink(),
            "the planted leaf is left as it was"
        );
        let leftovers = fs::read_dir(dir.path())
            .expect("dir")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".agendar-settings-")
            })
            .count();
        assert_eq!(leftovers, 1, "only the planted symlink remains");
    }

    #[cfg(unix)]
    #[test]
    fn an_exclusive_staging_open_refuses_a_symlink_leaf() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("dir");
        let victim = dir.path().join("victim.json");
        fs::write(&victim, b"untouched").expect("victim");
        let leaf = dir.path().join("staged.tmp");
        symlink(&victim, &leaf).expect("symlink leaf");

        let error = open_new_leaf(&leaf).expect_err("an existing leaf is never opened");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&victim).expect("victim"), b"untouched");
        assert!(
            fs::symlink_metadata(&leaf)
                .expect("leaf")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn the_plan_digest_changes_when_a_change_changes() {
        let plan = InstallPlan {
            harness: Some(Harness::ClaudeCode),
            settings_path: Some("/tmp/settings.json".to_owned()),
            changes: vec![HookChange {
                event: "Stop".to_owned(),
                command: "a hook stop".to_owned(),
                timeout_s: 60,
                async_rewake: false,
            }],
            requires_consent: true,
            plan_digest: Sha256::from_bytes(b""),
            already_installed: false,
            destination: None,
            command: None,
            cli_path: None,
        }
        .seal()
        .expect("seal");
        let mut other = plan.clone();
        other.changes[0].command = "b hook stop".to_owned();
        let other = other.seal().expect("seal");
        assert_ne!(plan.plan_digest, other.plan_digest);
    }
}

// ---------------------------------------------------------------------------
// Bindings: how each agent harness is connected to Agendar
// ---------------------------------------------------------------------------

/// How a harness is connected: hooks (Claude Code, Codex), the MCP server
/// (pi, opencode, Cursor, Antigravity, Gemini CLI), or by hand (dsh).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingKind {
    Hooks,
    Mcp,
    Manual,
}

/// Whether a harness is bound to this Agendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingState {
    Bound,
    NotBound,
    /// Bound to another Agendar executable (an old install path): rebind.
    Stale,
    /// The config file is not plain JSON; Agendar will not touch it.
    Unreadable,
    /// Bound by hand (dsh); Agendar cannot check it.
    Manual,
}

/// One harness's binding, for Settings and `agendar bindings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub harness: Harness,
    pub kind: BindingKind,
    pub state: BindingState,
    /// The harness's executable was found on PATH (or its home folder exists).
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub detail: String,
}

/// Every harness Agendar can bind, in display order.
pub const BINDABLE: [Harness; 8] = [
    Harness::ClaudeCode,
    Harness::Codex,
    Harness::Cursor,
    Harness::Gemini,
    Harness::Agy,
    Harness::OpenCode,
    Harness::Pi,
    Harness::Dsh,
];

/// The kind of binding a harness uses, when it has one.
pub fn binding_kind(harness: Harness) -> Option<BindingKind> {
    match harness {
        Harness::ClaudeCode | Harness::Codex => Some(BindingKind::Hooks),
        Harness::Pi | Harness::OpenCode | Harness::Cursor | Harness::Agy | Harness::Gemini => {
            Some(BindingKind::Mcp)
        }
        Harness::Dsh => Some(BindingKind::Manual),
        Harness::Generic | Harness::Herdr => None,
    }
}

/// The harness an agent's shell runs in, from the variables each harness sets
/// for its tools (for `agendar bind` without `--harness`).
pub fn detect_harness() -> Option<Harness> {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    // Most specific first: Cursor also runs Claude Code's hooks, and several
    // CLIs set the generic AGENT variable.
    if set("CURSOR_AGENT") || set("CURSOR_CONVERSATION_ID") {
        Some(Harness::Cursor)
    } else if set("ANTIGRAVITY_AGENT") {
        Some(Harness::Agy)
    } else if set("GEMINI_CLI") {
        Some(Harness::Gemini)
    } else if set("PI_CODING_AGENT") || set("PI_SESSION_ID") {
        Some(Harness::Pi)
    } else if set("OPENCODE") || set("OPENCODE_PID") {
        Some(Harness::OpenCode)
    } else if set("DSH_SHELL") || set("DSH_SESSION_ID") {
        Some(Harness::Dsh)
    } else if set("CODEX_THREAD_ID") {
        Some(Harness::Codex)
    } else if set("CLAUDECODE") || set("CLAUDE_CODE_ENTRYPOINT") {
        Some(Harness::ClaudeCode)
    } else {
        None
    }
}

fn hook_set_for(harness: Harness) -> Option<HookSet> {
    match harness {
        Harness::ClaudeCode => Some(HookSet::Skill),
        Harness::Codex => Some(HookSet::Codex),
        _ => None,
    }
}

/// An Agendar hook command of this set, for any Agendar executable path (so a
/// binding to an old path is recognised as stale, and unbinding removes it).
fn is_agendar_hook(set: HookSet, command: &str) -> bool {
    let actions: &[&str] = match set {
        HookSet::Skill => &[
            "hook session-start",
            "hook user-prompt-submit",
            "hook post-tool-use",
            "hook stop",
            "hook rewake",
        ],
        HookSet::Codex => &[
            "hook --harness codex session-start",
            "hook --harness codex user-prompt-submit",
            "hook --harness codex post-tool-use",
            "hook --harness codex stop",
        ],
        HookSet::Observe => &["hook observe"],
    };
    command.to_ascii_lowercase().contains("agendar")
        && actions
            .iter()
            .any(|action| command.trim_end().ends_with(action))
}

fn hook_commands_in(value: &Value) -> Vec<String> {
    let mut commands = Vec::new();
    if let Some(hooks) = value.get("hooks").and_then(Value::as_object) {
        for entries in hooks.values().filter_map(Value::as_array) {
            for entry in entries {
                if let Some(list) = entry.get("hooks").and_then(Value::as_array) {
                    commands.extend(
                        list.iter()
                            .filter_map(|hook| hook.get("command").and_then(Value::as_str))
                            .map(str::to_owned),
                    );
                }
            }
        }
    }
    commands
}

fn executable_found(harness: Harness, home: &Path) -> bool {
    let names: &[&str] = match harness {
        Harness::ClaudeCode => &["claude"],
        Harness::Codex => &["codex"],
        Harness::Pi => &["pi"],
        Harness::OpenCode => &["opencode"],
        Harness::Cursor => &["cursor-agent"],
        Harness::Agy => &["agy"],
        Harness::Gemini => &["gemini"],
        Harness::Dsh => &["dsh"],
        Harness::Generic | Harness::Herdr => &[],
    };
    let suffixes: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    let on_path = std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            names.iter().any(|name| {
                suffixes
                    .iter()
                    .any(|suffix| dir.join(format!("{name}{suffix}")).is_file())
            })
        })
    });
    // dsh usually runs from npx or a checkout: its home folder is the sign.
    on_path || (harness == Harness::Dsh && home.join(".dsh").is_dir())
}

/// The binding of one harness.
pub fn binding_status(harness: Harness, cli: &Path, home: &Path) -> Option<Binding> {
    let kind = binding_kind(harness)?;
    let found = executable_found(harness, home);
    let binding = |state, path: Option<PathBuf>, detail: String| Binding {
        harness,
        kind,
        state,
        found,
        path: path.map(|path| path.to_string_lossy().into_owned()),
        detail,
    };
    Some(match kind {
        BindingKind::Hooks => {
            let set = hook_set_for(harness)?;
            let path = hook_file_path(set, home, None);
            match read_settings(&path) {
                Err(error) => binding(
                    BindingState::Unreadable,
                    Some(path),
                    error.message().to_owned(),
                ),
                Ok(current) => {
                    let wanted = commands_for(set, cli).unwrap_or_default();
                    let present = hook_commands_in(&current);
                    let all = wanted
                        .iter()
                        .all(|change| present.iter().any(|command| command == &change.command));
                    let ours = present.iter().any(|command| is_agendar_hook(set, command));
                    if all {
                        binding(
                            BindingState::Bound,
                            Some(path),
                            "Hooks installed.".to_owned(),
                        )
                    } else if ours {
                        binding(
                            BindingState::Stale,
                            Some(path),
                            "Hooks point to another Agendar install or are incomplete; rebind."
                                .to_owned(),
                        )
                    } else {
                        binding(
                            BindingState::NotBound,
                            Some(path),
                            "No Agendar hooks.".to_owned(),
                        )
                    }
                }
            }
        }
        BindingKind::Mcp => {
            let Ok(target) = mcp_target(harness, cli, home) else {
                return None;
            };
            match read_settings(&target.path) {
                Err(error) => binding(
                    BindingState::Unreadable,
                    Some(target.path),
                    error.message().to_owned(),
                ),
                Ok(current) => match current
                    .get(target.servers_key)
                    .and_then(|servers| servers.get("agendar"))
                {
                    Some(entry) if entry == &target.entry => binding(
                        BindingState::Bound,
                        Some(target.path),
                        "MCP server added.".to_owned(),
                    ),
                    Some(_) => binding(
                        BindingState::Stale,
                        Some(target.path),
                        "The agendar MCP entry points elsewhere; rebind.".to_owned(),
                    ),
                    None => binding(
                        BindingState::NotBound,
                        Some(target.path),
                        "No agendar MCP server.".to_owned(),
                    ),
                },
            }
        }
        BindingKind::Manual => binding(
            BindingState::Manual,
            None,
            format!(
                "Add an MCP server row to dsh's mcp-client config: stdio, command `{}`, args `mcp --harness dsh`.",
                cli.to_string_lossy()
            ),
        ),
    })
}

/// Every bindable harness's binding.
pub fn bindings(cli: &Path, home: &Path) -> Vec<Binding> {
    BINDABLE
        .iter()
        .filter_map(|harness| binding_status(*harness, cli, home))
        .collect()
}

/// Bind one harness (hooks or MCP); a stale binding is replaced.
pub fn bind(harness: Harness, cli: &Path, home: &Path) -> Result<Binding> {
    match binding_kind(harness) {
        Some(BindingKind::Hooks) => {
            let set = hook_set_for(harness).expect("hooks kind has a set");
            unbind_hooks(set, home)?;
            let plan = plan_claude_hook_set(set, cli, None, home)?;
            install_claude_hook_set(set, cli, None, home, &plan.plan_digest)?;
        }
        Some(BindingKind::Mcp) => {
            let plan = plan_mcp(harness, cli, home)?;
            install_mcp(harness, cli, home, &plan.plan_digest)?;
        }
        Some(BindingKind::Manual) | None => {
            let detail = binding_status(harness, cli, home)
                .map(|binding| binding.detail)
                .unwrap_or_default();
            return Err(AgendarError::new(
                ErrorCode::Unsupported,
                "This harness is bound by hand.",
            )
            .with_detail("bind_manual")
            .with_hint(detail));
        }
    }
    binding_status(harness, cli, home)
        .ok_or_else(|| AgendarError::new(ErrorCode::Io, "The binding could not be read back."))
}

/// Remove Agendar's hooks or MCP entry for one harness; other entries stay.
pub fn unbind(harness: Harness, cli: &Path, home: &Path) -> Result<Binding> {
    match binding_kind(harness) {
        Some(BindingKind::Hooks) => {
            unbind_hooks(hook_set_for(harness).expect("hooks kind has a set"), home)?;
        }
        Some(BindingKind::Mcp) => {
            let target = mcp_target(harness, cli, home)?;
            let mut value = read_settings(&target.path)?;
            let removed = value
                .get_mut(target.servers_key)
                .and_then(Value::as_object_mut)
                .and_then(|servers| servers.remove("agendar"))
                .is_some();
            if removed {
                write_settings(&target.path, &value)?;
            }
        }
        Some(BindingKind::Manual) | None => {
            return Err(AgendarError::new(
                ErrorCode::Unsupported,
                "This harness is bound by hand; remove the row from its config.",
            )
            .with_detail("bind_manual"));
        }
    }
    binding_status(harness, cli, home)
        .ok_or_else(|| AgendarError::new(ErrorCode::Io, "The binding could not be read back."))
}

fn unbind_hooks(set: HookSet, home: &Path) -> Result<()> {
    let path = hook_file_path(set, home, None);
    let mut value = read_settings(&path)?;
    let mut changed = false;
    if let Some(hooks) = value.get_mut("hooks").and_then(Value::as_object_mut) {
        for entries in hooks.values_mut().filter_map(Value::as_array_mut) {
            for entry in entries.iter_mut() {
                if let Some(list) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
                    let before = list.len();
                    list.retain(|hook| {
                        !hook
                            .get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|command| is_agendar_hook(set, command))
                    });
                    changed |= list.len() != before;
                }
            }
            // Drop entries left without hooks.
            entries.retain(|entry| {
                entry
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_none_or(|list| !list.is_empty())
            });
        }
        hooks.retain(|_, entries| entries.as_array().is_none_or(|list| !list.is_empty()));
    }
    if changed {
        write_settings(&path, &value)?;
    }
    Ok(())
}

fn write_settings(path: &Path, value: &Value) -> Result<()> {
    refuse_symlinked_target(path)?;
    if path.exists() {
        backup_once(path)?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        AgendarError::new(
            ErrorCode::Io,
            format!("Cannot serialize the settings file: {error}."),
        )
        .with_detail("settings_serialize")
    })?;
    write_atomic(path, &bytes)
}
