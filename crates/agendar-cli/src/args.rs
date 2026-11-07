//! CLI grammar and context resolution.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Deserialize;

use agendar_core::error::{AgendarError, ErrorCode, Result};
use agendar_core::model::{EtaRange, Harness, Mode, ProjectSlug, Seconds, SessionId};
use agendar_core::runtime::Config;

/// Agendar command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "agendar",
    version,
    about = "Project radar and one-question decision loop for coding agents."
)]
pub struct Cli {
    /// Vault root (overrides AGENDAR_VAULT and the selected local config).
    #[arg(long, global = true, value_name = "PATH")]
    pub vault: Option<PathBuf>,
    /// Local data directory (overrides AGENDAR_DATA_DIR).
    #[arg(long, global = true, value_name = "PATH")]
    pub data_dir: Option<PathBuf>,
    /// Project slug (overrides AGENDAR_PROJECT and the selected local config).
    #[arg(long, global = true, value_name = "SLUG")]
    pub project: Option<String>,
    /// Session ULID (overrides AGENDAR_SESSION; never read from selected config).
    #[arg(long, global = true, value_name = "ULID")]
    pub session: Option<String>,
    /// Any short key you repeat on a retry (e.g. start-core-wake-1).
    #[arg(long, global = true, value_name = "KEY")]
    pub request_id: Option<String>,
    /// Read the last committed projection instead of ingesting first.
    #[arg(long, global = true)]
    pub cached: bool,
    /// Human diagnostics on stderr (never mixed with JSON output).
    #[arg(long, global = true)]
    pub verbose: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize the Vault, index and local host identity.
    Init {
        #[arg(long, value_enum)]
        harness: Option<HarnessArg>,
        #[arg(long, value_name = "ID")]
        harness_session: Option<String>,
        #[arg(long, value_name = "PID")]
        pid: Option<u32>,
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
    },
    /// Project declarations.
    Project {
        #[command(subcommand)]
        action: ProjectCommand,
    },
    /// Feature declarations.
    Feature {
        #[command(subcommand)]
        action: FeatureCommand,
    },
    /// Work progress.
    Progress {
        #[command(subcommand)]
        action: ProgressCommand,
    },
    /// One progress pulse (non-boundary pulses are rate limited).
    Pulse {
        #[arg(long, value_name = "PATH")]
        node: String,
        #[arg(long, value_name = "ULID")]
        attempt: String,
        #[arg(long, value_name = "TEXT")]
        message: String,
        #[arg(long)]
        boundary: bool,
        #[arg(long, value_name = "MIN..MAX")]
        eta: Option<String>,
    },
    /// Ask a decision question and return immediately.
    Ask {
        /// The step it is about; defaults to the step you are doing.
        #[arg(long, value_name = "PATH")]
        node: Option<String>,
        /// A full card (see `agendar schema card`); or use the quick flags.
        #[arg(long, value_name = "FILE|-", conflicts_with = "question")]
        json: Option<String>,
        /// Quick form: the question (≤80), with two to four --option.
        #[arg(long, value_name = "TEXT")]
        question: Option<String>,
        /// Quick form: "Label: what happens if chosen" (label ≤40, rest ≤90).
        #[arg(long = "option", value_name = "TEXT")]
        options: Vec<String>,
        /// Quick form: the option you recommend (its number, from 1).
        #[arg(long, value_name = "N")]
        recommend: Option<usize>,
        /// Quick form: why it is needed now (≤100).
        #[arg(long, value_name = "TEXT")]
        why: Option<String>,
        /// `ask` reaches the human; `assume`/`auto` let their policy answer
        /// easy, local questions without them.
        #[arg(long, value_enum, default_value_t = ModeArg::Ask)]
        mode: ModeArg,
    },
    /// Read pending packets and house rules; never acknowledges.
    Inbox {
        #[arg(long, value_name = "PATH")]
        node: Option<String>,
    },
    /// Block until a current answer exists, then print it once.
    Wait {
        ask_id: String,
        #[arg(long, value_name = "DURATION")]
        timeout: Option<String>,
    },
    /// Acknowledge a current answer.
    Ack {
        answer_id: String,
        #[arg(long = "restarted", value_name = "PATH")]
        restarted: Vec<String>,
    },
    /// Withdraw an unacked decision you asked.
    Withdraw {
        ask_id: String,
        #[arg(long, value_name = "TEXT")]
        reason: String,
    },
    /// Publish a report: a short summary in the app, the full document (html,
    /// md or txt) in the Vault, opened in the human's browser.
    Report {
        #[arg(long, value_name = "TEXT")]
        title: String,
        #[arg(long, value_name = "TEXT")]
        summary: String,
        /// The document; its extension (html, md, txt) is its type.
        #[arg(long, value_name = "FILE")]
        file: String,
        /// The step it is about, when there is one.
        #[arg(long, value_name = "PATH")]
        node: Option<String>,
    },
    /// Publish an attachment blob or deliver a feature's completed work.
    Blob {
        #[command(subcommand)]
        action: BlobCommand,
    },
    /// Upload demo/evidence files first, then append one delivery event.
    Deliver {
        #[arg(long, value_name = "PATH")]
        node: String,
        #[arg(long, value_name = "ULID")]
        attempt: String,
        #[arg(long, value_name = "ULID")]
        receipt: String,
        #[arg(long, value_name = "FILE|-")]
        json: String,
        #[arg(long, value_name = "ULID")]
        supersedes: Option<String>,
    },
    /// Enabled house rules.
    Rules,
    /// The project as an arriving agent needs it: declared or not, who works on
    /// which step, what you own and what you may start. Read-only.
    Board,
    /// Local Vault/index health.
    Status,
    /// Vault/index maintenance.
    Index {
        #[command(subcommand)]
        action: IndexCommand,
    },
    /// Canonical state utilities.
    State {
        #[command(subcommand)]
        action: StateCommand,
    },
    /// Exact JSON shapes, enums, text limits and icon names for agent inputs.
    Schema {
        #[arg(value_parser = ["card", "feature", "project", "eta"])]
        kind: Option<String>,
    },
    /// Embedded agent skill.
    Skill {
        #[command(subcommand)]
        action: SkillCommand,
    },
    /// Run one host-local steward tick.
    Steward {
        #[arg(long)]
        once: bool,
    },
    /// Internal Claude Code hook protocol (stdin JSON, stdout JSON).
    Hook {
        /// Which harness runs the hook (its hook file names it).
        #[arg(long, value_enum, default_value = "claude_code")]
        harness: HarnessArg,
        #[command(subcommand)]
        action: HookCommand,
    },
    /// Serve Agendar as a stdio MCP server (the adapter for MCP-capable
    /// harnesses): typed tools over the same commands.
    Mcp {
        /// The harness that loads this server (names its sessions).
        #[arg(long, value_enum, default_value = "generic")]
        harness: HarnessArg,
    },
    /// Connect the agent harness you run in to Agendar (hooks for Claude Code
    /// and Codex, the MCP server for the others). Detects the harness when
    /// --harness is omitted.
    Bind {
        #[arg(long, value_enum)]
        harness: Option<HarnessArg>,
    },
    /// Remove Agendar's hooks or MCP entry from one harness.
    Unbind {
        #[arg(long, value_enum)]
        harness: HarnessArg,
    },
    /// How each agent harness on this machine is bound. Read-only.
    Bindings,
    /// Plan or add the `agendar` MCP server to a harness's MCP config
    /// (pi, opencode, cursor, agy, gemini); dry-run unless --yes.
    InstallMcp {
        #[arg(long, value_enum)]
        harness: HarnessArg,
        /// Write the config file; the default is a dry-run plan.
        #[arg(long)]
        yes: bool,
    },
    /// Plan or install Claude Code hooks (dry-run unless --yes).
    InstallHooks {
        #[arg(long, value_enum, default_value = "claude_code")]
        harness: HarnessArg,
        /// Write the settings file; the default is a dry-run plan.
        #[arg(long)]
        yes: bool,
        /// Explicit settings file override (default: $HOME/.claude/settings.json).
        #[arg(long, value_name = "PATH")]
        settings: Option<PathBuf>,
        /// Refuse writes even when --yes was mistakenly supplied.
        #[arg(long)]
        dry_run: bool,
    },
}

impl Command {
    /// Whether `--cached` is meaningful for this command. Mutations always
    /// ingest and never claim cached freshness.
    pub const fn is_read(&self) -> bool {
        matches!(
            self,
            Self::Inbox { .. }
                | Self::Wait { .. }
                | Self::Rules
                | Self::Board
                | Self::Status
                | Self::State { .. }
        )
    }
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Declare or patch the project.
    Declare {
        #[arg(long, value_name = "FILE|-")]
        json: String,
    },
    /// Link a repository to a project in this host's local config.
    Link {
        #[arg(long, value_name = "PATH")]
        repo: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum FeatureCommand {
    /// Declare or patch a feature.
    Declare {
        #[arg(long, value_name = "PATH")]
        node: String,
        #[arg(long, value_name = "FILE|-")]
        json: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProgressCommand {
    /// Start an attempt with an ETA.
    Started {
        #[arg(long, value_name = "PATH")]
        node: String,
        #[arg(long, value_name = "MIN..MAX")]
        eta: String,
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
        /// An answer this work applies (repeatable); no ack needed first.
        #[arg(long, value_name = "ANSWER_ID")]
        applies: Vec<String>,
    },
    /// Mark the current attempt done.
    Done {
        #[arg(long, value_name = "PATH")]
        node: String,
        #[arg(long, value_name = "ULID")]
        attempt: String,
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
    },
    /// Drop the current attempt, or a step nobody started (omit --attempt).
    Dropped {
        #[arg(long, value_name = "PATH")]
        node: String,
        #[arg(long, value_name = "ULID")]
        attempt: Option<String>,
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
    },
    /// Mark the current attempt verified with evidence.
    Verified {
        #[arg(long, value_name = "PATH")]
        node: String,
        #[arg(long, value_name = "ULID")]
        attempt: String,
        #[arg(long, value_name = "FILE|-")]
        json: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum BlobCommand {
    /// Publish one local file as a create-only content-addressed blob.
    Put {
        #[arg(value_name = "FILE")]
        file: PathBuf,
        #[arg(long, value_name = "EXT")]
        ext: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum IndexCommand {
    /// Rebuild the disposable index in place.
    Rebuild,
}

#[derive(Debug, Subcommand)]
pub enum StateCommand {
    /// Emit canonical State at one evaluation time (raw JSON, no envelope).
    Export {
        #[arg(long, value_name = "UTC")]
        as_of: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// Print the exact embedded skill bytes.
    Print,
    /// Report the embedded skill version and digest.
    Info,
}

#[derive(Debug, Subcommand)]
pub enum HookCommand {
    SessionStart,
    /// A human prompt: new answers and reminders go into the agent's context.
    UserPromptSubmit,
    PostToolUse,
    Stop,
    /// Background (Claude Code `asyncRewake`): after a turn ends with a
    /// question open, wait for its answer and wake the session with it.
    Rewake,
    /// Observe-only: record whether this agent works or waits; prints nothing.
    Observe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HarnessArg {
    #[value(name = "claude_code")]
    ClaudeCode,
    Codex,
    Generic,
    Pi,
    Herdr,
    #[value(name = "opencode")]
    OpenCode,
    Cursor,
    Dsh,
    Agy,
    Gemini,
}

impl From<HarnessArg> for Harness {
    fn from(value: HarnessArg) -> Self {
        match value {
            HarnessArg::ClaudeCode => Harness::ClaudeCode,
            HarnessArg::Codex => Harness::Codex,
            HarnessArg::Generic => Harness::Generic,
            HarnessArg::Pi => Harness::Pi,
            HarnessArg::Herdr => Harness::Herdr,
            HarnessArg::OpenCode => Harness::OpenCode,
            HarnessArg::Cursor => Harness::Cursor,
            HarnessArg::Dsh => Harness::Dsh,
            HarnessArg::Agy => Harness::Agy,
            HarnessArg::Gemini => Harness::Gemini,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ModeArg {
    Auto,
    Assume,
    Ask,
}

impl From<ModeArg> for Mode {
    fn from(value: ModeArg) -> Self {
        match value {
            ModeArg::Auto => Mode::Auto,
            ModeArg::Assume => Mode::Assume,
            ModeArg::Ask => Mode::Ask,
        }
    }
}

/// Resolved invocation context.
#[derive(Debug, Clone)]
pub struct Context {
    pub vault: PathBuf,
    pub data_dir: PathBuf,
    pub project: Option<ProjectSlug>,
    /// True when the project came from an explicit flag or environment value,
    /// not from the working directory's repository link.
    pub project_explicit: bool,
    pub session: Option<SessionId>,
    pub request_id: Option<String>,
    pub cached: bool,
    pub verbose: bool,
}

impl Context {
    /// Project or the typed context error.
    pub fn require_project(&self) -> Result<ProjectSlug> {
        self.project.clone().ok_or_else(|| {
            AgendarError::new(ErrorCode::Context, "Missing project.")
                .with_detail("missing_project")
                .with_hint(format!(
                    "Run `{} project link --repo <path> --project <slug>` from the repository, or pass --project.",
                    agendar_core::service::cli_token()
                ))
        })
    }

    /// Session or the typed context error.
    pub fn require_session(&self) -> Result<SessionId> {
        self.session.clone().ok_or_else(|| {
            AgendarError::new(ErrorCode::Context, "Missing session.")
                .with_detail("missing_session")
                .with_hint(format!(
                    "Run `{} init` or supply --session.",
                    agendar_core::service::cli_token()
                ))
        })
    }
}

/// Resolve the local data directory from flags, environment or the OS default.
pub fn resolve_data_root(cli: &Cli) -> Result<PathBuf> {
    match &cli.data_dir {
        Some(path) => Ok(path.clone()),
        None => match std::env::var_os("AGENDAR_DATA_DIR") {
            Some(value) => Ok(PathBuf::from(value)),
            None => agendar_core::runtime::LocalPaths::default_data_root(),
        },
    }
}

/// Selected Vault path from the flag, the environment or the local config.
/// `None` means the invocation selects no Vault: setup commands such as
/// `project link` run before one exists.
pub fn resolve_vault(cli: &Cli, config: &Config) -> Option<PathBuf> {
    if let Some(path) = &cli.vault {
        return Some(path.clone());
    }
    if let Some(value) = std::env::var_os("AGENDAR_VAULT") {
        return Some(PathBuf::from(value));
    }
    config.vault.as_ref().map(PathBuf::from)
}

/// Resolve flags, environment and the selected local configuration.
pub fn resolve(cli: &Cli) -> Result<Context> {
    let data_dir = resolve_data_root(cli)?;
    let config = if data_dir.exists() {
        Config::load_or_default(&data_dir)?
    } else {
        Config::default()
    };
    let vault = resolve_vault(cli, &config).ok_or_else(|| {
        AgendarError::new(ErrorCode::Context, "Missing Vault.")
            .with_detail("missing_vault")
            .with_hint(format!(
                "Run `{} init` or supply --vault.",
                agendar_core::service::cli_token()
            ))
    })?;
    // Resolution order: explicit flag, environment override, then the longest
    // linked repository prefix containing the session's working directory. A
    // selected project is never inferred from a global config value: the repo
    // link is the durable mapping a hook and a terminal share.
    let (project, project_explicit) = match &cli.project {
        Some(value) => (Some(ProjectSlug::parse(value)?), true),
        None => match std::env::var("AGENDAR_PROJECT") {
            Ok(value) if !value.trim().is_empty() => (Some(ProjectSlug::parse(&value)?), true),
            _ => (
                std::env::current_dir()
                    .ok()
                    .and_then(|cwd| config.project_for_cwd(&cwd)),
                false,
            ),
        },
    };
    let session = match &cli.session {
        Some(value) => Some(SessionId::parse(value)?),
        None => match std::env::var("AGENDAR_SESSION") {
            Ok(value) if !value.trim().is_empty() => Some(SessionId::parse(&value)?),
            _ => None,
        },
    };
    let request_id = match &cli.request_id {
        Some(value) => Some(parse_request_id(value)?),
        None => None,
    };
    Ok(Context {
        vault,
        data_dir,
        project,
        project_explicit,
        session,
        request_id,
        cached: cli.cached,
        verbose: cli.verbose,
    })
}

/// Parse `MIN..MAX` durations (`30m..1h`) into seconds.
pub fn parse_eta(value: &str) -> Result<EtaRange> {
    let (minimum, maximum) = value.split_once("..").ok_or_else(|| {
        AgendarError::new(ErrorCode::Usage, "Invalid argument: --eta.")
            .with_field("/eta")
            .with_detail("eta_syntax")
            .with_hint("Example: --eta 30m..1h")
    })?;
    let range = EtaRange {
        min_s: parse_duration_seconds(minimum, "/eta/min_s")?,
        max_s: parse_duration_seconds(maximum, "/eta/max_s")?,
    };
    range.validate("/eta")?;
    Ok(range)
}

/// A request id is any short key the agent can repeat on a retry, such as
/// `start-core-wake-1` (a ULID still works). The journal scopes it to the
/// session, so two agents picking the same key never collide.
pub fn parse_request_id(value: &str) -> Result<String> {
    let valid = (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'));
    if valid {
        Ok(value.to_owned())
    } else {
        Err(AgendarError::new(ErrorCode::Usage, "Invalid argument: --request-id.")
            .with_field("/request_id")
            .with_detail("request_id_syntax")
            .with_hint(
                "Use 1..=64 letters, digits, `.`, `_`, `:` or `-` that you can repeat on a retry, \
                 such as start-core-wake-1.",
            ))
    }
}

/// Parse a CLI duration (`45s`, `5m`, `1h`, `2d`, or compound `1h30m`).
pub fn parse_duration(value: &str) -> Result<Duration> {
    let seconds = parse_duration_seconds(value, "/timeout")?;
    Ok(Duration::from_secs(u64::from(seconds.get())))
}

/// One or more `<amount><unit>` parts with units in d, h, m, s order, each at
/// most once: `90m`, `1h30m`, `2d4h`, `1h05m`. The first amount has no leading
/// zero; a later one may be zero-padded to two digits.
fn parse_duration_seconds(value: &str, field: &str) -> Result<Seconds> {
    const UNITS: [(u8, u32); 4] = [(b'd', 86_400), (b'h', 3_600), (b'm', 60), (b's', 1)];
    let bytes = value.as_bytes();
    let mut total: u32 = 0;
    let mut next_unit = 0;
    let mut start = 0;
    while start < bytes.len() {
        let digits_end = bytes[start..]
            .iter()
            .position(|byte| !byte.is_ascii_digit())
            .map(|offset| start + offset)
            .ok_or_else(|| duration_error(value, field))?;
        let digits = &value[start..digits_end];
        let padded = start > 0 && digits.len() == 2;
        if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0') && !padded) {
            return Err(duration_error(value, field));
        }
        let unit = bytes[digits_end];
        let Some(index) = UNITS[next_unit..]
            .iter()
            .position(|(name, _)| *name == unit)
            .map(|offset| next_unit + offset)
        else {
            return Err(duration_error(value, field));
        };
        let amount: u32 = digits.parse().map_err(|_| duration_error(value, field))?;
        total = amount
            .checked_mul(UNITS[index].1)
            .and_then(|seconds| total.checked_add(seconds))
            .ok_or_else(|| duration_error(value, field))?;
        next_unit = index + 1;
        start = digits_end + 1;
    }
    if start == 0 {
        return Err(duration_error(value, field));
    }
    Seconds::new(total).map_err(|_| duration_error(value, field))
}

fn duration_error(value: &str, field: &str) -> AgendarError {
    AgendarError::new(ErrorCode::Usage, format!("Invalid argument: {field}."))
        .with_field(field)
        .with_detail("duration_syntax")
        .with_hint(format!(
            "{field} must be a positive whole duration such as 30s, 5m, 1h, 1h30m or 2d; got `{value}`."
        ))
}

/// Typed input for `progress verified`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedInput {
    pub evidence: Vec<agendar_core::model::CriterionResult>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eta_ranges_use_the_documented_duration_grammar() {
        let range = parse_eta("30m..1h").expect("valid range");
        assert_eq!(range.min_s.get(), 1_800);
        assert_eq!(range.max_s.get(), 3_600);
        assert!(parse_eta("1h..30m").is_err(), "reversed range");
        assert!(parse_eta("0s..1h").is_err(), "zero minimum");
        assert!(parse_eta("1.5h..2h").is_err(), "decimals are rejected");
        assert!(parse_eta("01m..2m").is_err(), "leading zero is rejected");
        assert!(parse_eta("1h").is_err(), "missing range separator");
    }

    #[test]
    fn request_ids_are_short_repeatable_keys() {
        assert_eq!(
            parse_request_id("start-core-wake-1").expect("key"),
            "start-core-wake-1"
        );
        assert!(
            parse_request_id("01J00000000000000000000001").is_ok(),
            "a ULID works"
        );
        assert!(parse_request_id("ask:core/wake").is_err(), "no slashes");
        assert!(parse_request_id("two words").is_err());
        assert!(parse_request_id("").is_err());
        assert!(parse_request_id(&"k".repeat(65)).is_err());
    }

    #[test]
    fn compound_durations_add_their_parts_in_unit_order() {
        let range = parse_eta("45m..1h30m").expect("compound range");
        assert_eq!(range.max_s.get(), 5_400);
        assert_eq!(parse_duration("2d4h").expect("2d4h").as_secs(), 187_200);
        assert_eq!(parse_duration("1h05m").expect("padded").as_secs(), 3_900);
        assert_eq!(parse_duration("1m30s").expect("1m30s").as_secs(), 90);
        assert!(parse_duration("30m1h").is_err(), "units must descend");
        assert!(parse_duration("1h1h").is_err(), "a unit appears once");
        assert!(
            parse_duration("1h005m").is_err(),
            "pad to two digits at most"
        );
        assert!(parse_duration("1h30").is_err(), "every part needs a unit");
        assert!(
            parse_duration("h30m").is_err(),
            "every part needs an amount"
        );
        assert!(parse_duration("0h0m").is_err(), "zero total");
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn wait_timeouts_use_the_same_grammar() {
        assert_eq!(parse_duration("2d").expect("two days").as_secs(), 172_800);
        assert_eq!(parse_duration("45s").expect("45 seconds").as_secs(), 45);
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("-5s").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn hook_prefix_quoting_survives_spaces_and_quotes() {
        use agendar_core::service::shell_quote;
        assert_eq!(shell_quote("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
