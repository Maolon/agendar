//! Shared CLI test harness: a scratch Vault, local data root and HOME.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

use agendar_core::model::{EventId, OptionId, ProjectSlug};
use agendar_core::service::Service;

/// One isolated CLI environment.
pub struct CliEnv {
    pub root: TempDir,
    pub vault: PathBuf,
    pub data: PathBuf,
    pub repo: PathBuf,
    pub home: PathBuf,
    pub project: String,
    pub session: String,
    pub host: String,
}

impl CliEnv {
    /// Create the scratch directories; no Vault is initialized yet.
    pub fn new(project: &str) -> Self {
        let root = tempfile::tempdir().expect("temporary test root");
        let vault = root.path().join("vault");
        let data = root.path().join("data");
        let repo = root.path().join("repo");
        let home = root.path().join("home");
        fs::create_dir_all(&repo).expect("repo");
        fs::create_dir_all(&home).expect("home");
        // Windows resolves the app data folders from USERPROFILE and only when
        // they exist, as they do on a real machine.
        for dir in ["AppData/Roaming", "AppData/Local"] {
            fs::create_dir_all(home.join(dir)).expect("app data dir");
        }
        Self {
            root,
            vault,
            data,
            repo,
            home,
            project: project.to_owned(),
            session: String::new(),
            host: String::new(),
        }
    }

    /// Build a command with a clean environment and the explicit scratch paths.
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agendar"));
        for key in [
            "AGENDAR_VAULT",
            "AGENDAR_DATA_DIR",
            "AGENDAR_PROJECT",
            "AGENDAR_SESSION",
            "AGENDAR_HOOK_PID",
            "AGENDAR_HOOK_BUDGET_MS",
            "AGENDAR_HARNESS_EXE",
            "CODEX_THREAD_ID",
            // Never let a test reach the operator's real agent settings.
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "USERPROFILE",
            "CURSOR_VERSION",
            "XDG_CONFIG_HOME",
            "PI_CODING_AGENT_DIR",
            "GEMINI_CLI_HOME",
            // Harness markers: tests run inside a real agent's shell.
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CURSOR_AGENT",
            "CURSOR_CONVERSATION_ID",
            "ANTIGRAVITY_AGENT",
            "GEMINI_CLI",
            "PI_CODING_AGENT",
            "PI_SESSION_ID",
            "OPENCODE",
            "OPENCODE_PID",
            "DSH_SHELL",
            "DSH_SESSION_ID",
        ] {
            command.env_remove(key);
        }
        command
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("APPDATA", self.home.join("AppData/Roaming"))
            .env("LOCALAPPDATA", self.home.join("AppData/Local"))
            .env("USERPROFILE", &self.home)
            .env("NO_COLOR", "1")
            .current_dir(&self.repo);
        command
    }

    /// Run one command with the standard context prefix.
    pub fn run(&self, args: &[&str]) -> Output {
        let mut command = self.command();
        command
            .arg("--vault")
            .arg(&self.vault)
            .arg("--data-dir")
            .arg(&self.data)
            .arg("--project")
            .arg(&self.project);
        if !self.session.is_empty() {
            command.arg("--session").arg(&self.session);
        }
        command.args(args);
        command.output().expect("run agendar")
    }

    /// Run with extra environment variables (hook tests, budget overrides).
    pub fn run_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = self.command();
        command
            .arg("--vault")
            .arg(&self.vault)
            .arg("--data-dir")
            .arg(&self.data)
            .arg("--project")
            .arg(&self.project);
        if !self.session.is_empty() {
            command.arg("--session").arg(&self.session);
        }
        command.args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().expect("run agendar")
    }

    /// Run one command as an explicit session (hook-registered sessions).
    pub fn run_as(&self, session: &str, args: &[&str]) -> Output {
        let mut command = self.command();
        command
            .arg("--vault")
            .arg(&self.vault)
            .arg("--data-dir")
            .arg(&self.data)
            .arg("--project")
            .arg(&self.project)
            .arg("--session")
            .arg(session)
            .args(args);
        command.output().expect("run agendar")
    }

    /// Run init and register a generic harness session using this test process.
    pub fn init(&mut self) {
        // A fresh registration: `init --session X` would keep session X.
        self.session.clear();
        let harness_session = format!("harness-{}", ulid::Ulid::generate());
        let pid = std::process::id().to_string();
        let repo = self.repo.to_string_lossy().into_owned();
        let output = self.run(&[
            "init",
            "--harness",
            "generic",
            "--harness-session",
            &harness_session,
            "--pid",
            &pid,
            "--repo",
            &repo,
        ]);
        let value = expect_success(&output, "init");
        self.session = value["data"]["session"]
            .as_str()
            .expect("init session")
            .to_owned();
        self.host = value["data"]["host"]
            .as_str()
            .expect("init host")
            .to_owned();
    }

    /// Open the same Vault through the shared service (used to append a human
    /// answer, which is an operator action and not an agent CLI command).
    pub fn service(&self) -> Service {
        Service::open(&self.vault, &self.data).expect("open scratch service")
    }

    /// Every complete Vault event line, for no-write and duplicate assertions.
    pub fn event_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        collect_jsonl(&self.vault.join("projects"), &mut lines);
        lines
    }

    /// Count events of one wire kind in the scratch Vault.
    pub fn count_kind(&self, kind: &str) -> usize {
        let needle = format!("\"kind\":\"{kind}\"");
        self.event_lines()
            .into_iter()
            .filter(|line| line.contains(&needle))
            .count()
    }

    /// Path to a committed test fixture.
    pub fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// Append a human answer through the shared service.
    pub fn answer(&self, ask_id: &str, option: &str) -> String {
        let service = self.service();
        let project = ProjectSlug::parse(&self.project).expect("project");
        let ask = EventId::parse(ask_id).expect("ask id");
        let option = OptionId::parse(option).expect("option");
        service
            .answer_decision(&project, &ask, &option, None, None, None)
            .expect("append human answer")
            .as_str()
            .to_owned()
    }
}

fn collect_jsonl(directory: &Path, lines: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, lines);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            let text = fs::read_to_string(&path).unwrap_or_default();
            lines.extend(
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(str::to_owned),
            );
        }
    }
}

/// Assert exit 0, a success envelope and return the parsed stdout.
pub fn expect_success(output: &Output, command: &str) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "command {command} failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let first = lines.next().unwrap_or_default();
    assert!(
        lines.next().is_none(),
        "success must be exactly one JSON line: {stdout}"
    );
    let value: Value = serde_json::from_str(first).expect("success JSON");
    assert_eq!(value["v"], 1);
    assert_eq!(value["ok"], true);
    assert_eq!(value["command"], command);
    assert!(
        value.get("freshness").is_some(),
        "success envelope has freshness"
    );
    value
}

/// Assert a typed error envelope and its exit code.
pub fn expect_error(output: &Output, code: &str, exit: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "unexpected exit; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "errors must not write stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut lines = stderr.lines();
    let first = lines.next().unwrap_or_default();
    assert!(
        lines.next().is_none(),
        "error must be exactly one JSON line: {stderr}"
    );
    let value: Value = serde_json::from_str(first).expect("error JSON");
    assert_eq!(value["v"], 1);
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], code);
    assert!(value["error"]["message"].is_string());
    value
}

/// Assert stdout is exactly one raw JSON document (`state export`).
pub fn expect_raw_json(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "raw command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).expect("raw JSON")
}
