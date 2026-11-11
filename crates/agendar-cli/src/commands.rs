//! Command dispatch into the shared service.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use agendar_core::adapters::Adapter;
use agendar_core::board::BoardView;
use agendar_core::error::{AgendarError, ErrorCode, Result};
use agendar_core::model::{
    BlobExt, CriterionId, DecisionCard, DeliveredOutcome, EventId, Harness, NodePath, PlanFeature,
    ProjectDeclared, Sha256, UtcTime, WorkStatus,
};
use agendar_core::runtime::sessions::process_identity;
use agendar_core::runtime::steward::steward_tick;
use agendar_core::runtime::{SessionRegistration, register_session};
use agendar_core::service::{Freshness, Service, WaitOptions, WaitTarget};
use agendar_core::vault::Clock;
use agendar_core::vault::SystemClock;

use crate::args::{
    BlobCommand, Cli, Command, Context, FeatureCommand, HookCommand, IndexCommand, ProgressCommand,
    ProjectCommand, SkillCommand, StateCommand, VerifiedInput, parse_duration, parse_eta, resolve,
};
use crate::delivery::{self, BlobInput, DeliveryInput, parse_blob_ext};
use crate::hooks::HookInput;
use crate::output;

/// Maximum local JSON input object.
pub const MAX_JSON_INPUT_BYTES: usize = 65_536;

/// Dispatch one parsed invocation.
/// How long an agent command waits for a local lock that other Agendar
/// processes hold before it gives up with `E_BUSY`.
const CLI_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

pub fn dispatch(cli: Cli) -> i32 {
    // An agent command waits out the other processes' refreshes instead of
    // failing after the app's short wait. Hooks keep the short wait: they run
    // inside the harness's own time budget, often in front of the user.
    if !matches!(cli.command, Command::Hook { .. }) {
        agendar_core::runtime::set_lock_wait(CLI_LOCK_WAIT);
    }
    // The MCP server resolves context per tool call (it runs this CLI).
    if let Command::Mcp { harness } = &cli.command {
        return crate::mcp::run((*harness).into());
    }
    if let Command::InstallMcp { harness, yes } = &cli.command {
        return run_install_mcp((*harness).into(), *yes);
    }
    match &cli.command {
        Command::Bind { harness } => return run_bind(harness.map(Into::into), true),
        Command::Unbind { harness } => return run_bind(Some((*harness).into()), false),
        Command::Bindings => return run_bindings(),
        _ => {}
    }
    // Installation is a local setup action; it must work before a Vault exists.
    if let Command::InstallHooks {
        harness,
        yes,
        settings,
        dry_run,
    } = &cli.command
    {
        return run_install_hooks(*harness, *yes, settings.clone(), *dry_run);
    }
    // The observe hook runs for agents without the skill: no Vault, no project,
    // never a word on stdout, never a failure the harness would notice.
    if let Command::Hook {
        action: HookCommand::Observe,
        ..
    } = &cli.command
    {
        return run_observe(cli.data_dir.as_deref());
    }
    // The embedded skill is a bundled asset. Agents read it as a fallback before
    // any Vault or local config exists, so it never resolves a context.
    if let Command::Skill { action } = &cli.command {
        return run_skill(action);
    }
    // The input reference is static, like the skill.
    if let Command::Schema { kind } = &cli.command {
        return match agendar_core::model::schema::schema(kind.as_deref()) {
            Some(value) => output::success("schema", value, None),
            None => 2,
        };
    }
    // The update check reads and writes the local data directory only.
    if let Command::Update { .. } = &cli.command {
        return run_update(&cli);
    }
    // Repository links are local config only; they must work before a Vault
    // exists on this host.
    if let Command::Project {
        action: ProjectCommand::Link { repo },
    } = &cli.command
    {
        return run_project_link(&cli, repo);
    }
    let mut context = match resolve(&cli) {
        Ok(context) => context,
        Err(error) => return output::error(&error),
    };
    if !cli.command.is_read() {
        context.cached = false;
    }
    match cli.command {
        Command::Init {
            harness,
            harness_session,
            pid,
            repo,
        } => run_init(&context, harness, harness_session, pid, repo),
        Command::Project { action } => match action {
            ProjectCommand::Declare { json } => run_project_declare(&context, &json),
            // Handled before context resolution; unreachable here.
            ProjectCommand::Link { .. } => 2,
        },
        Command::Feature { action } => match action {
            FeatureCommand::Declare { node, json } => run_feature_declare(&context, &node, &json),
        },
        Command::Progress { action } => run_progress(&context, action),
        Command::Pulse {
            node,
            attempt,
            message,
            boundary,
            eta,
        } => run_pulse(&context, node, attempt, message, boundary, eta),
        Command::Ask {
            node,
            json,
            question,
            options,
            recommend,
            why,
            takeover,
            mode,
        } => {
            let quick = QuickAsk {
                question,
                options,
                recommend,
                why,
                takeover,
            };
            run_ask(
                &context,
                node.as_deref(),
                json.as_deref(),
                quick,
                mode.into(),
            )
        }
        Command::Inbox { node } => run_inbox(&context, node),
        Command::Wait {
            ask_id,
            delivery,
            timeout,
        } => run_wait(&context, ask_id.as_deref(), delivery.as_deref(), timeout),
        Command::Ack {
            answer_id,
            restarted,
        } => run_ack(&context, &answer_id, restarted),
        Command::Withdraw { ask_id, reason } => run_withdraw(&context, &ask_id, reason),
        Command::Report {
            title,
            summary,
            file,
            node,
            kind,
            domain,
        } => run_report(
            &context,
            title,
            summary,
            &file,
            node.as_deref(),
            kind.map(Into::into),
            domain.map(Into::into),
        ),
        Command::Blob { action } => match action {
            BlobCommand::Put { file, ext } => run_blob_put(&context, &file, &ext),
        },
        Command::Deliver {
            node,
            attempt,
            receipt,
            json,
            supersedes,
        } => run_deliver(
            &context,
            &node,
            &attempt,
            receipt.as_deref(),
            &json,
            supersedes.as_deref(),
        ),
        Command::Rules => run_rules(&context),
        Command::Board {
            milestone,
            all,
            out,
        } => {
            let view = match (milestone, all) {
                (Some(name), _) => BoardView::Milestone(Some(name).filter(|name| !name.is_empty())),
                (None, true) => BoardView::All,
                (None, false) => BoardView::Open,
            };
            run_board(&context, &view, out.as_deref())
        }
        Command::Settle => run_settle(&context),
        Command::Status => run_status(&context),
        Command::Prune => run_prune(&context),
        Command::Index { action } => match action {
            IndexCommand::Rebuild => run_rebuild(&context),
        },
        Command::State { action } => match action {
            StateCommand::Export { as_of } => run_state_export(&context, &as_of),
        },
        Command::Steward { once } => run_steward(&context, once),
        Command::Hook { harness, action } => {
            crate::hooks::set_harness(harness.into());
            run_hook(&context, action)
        }
        // Handled before context resolution; unreachable here.
        Command::InstallHooks { .. }
        | Command::Skill { .. }
        | Command::Schema { .. }
        | Command::Mcp { .. }
        | Command::InstallMcp { .. }
        | Command::Bind { .. }
        | Command::Unbind { .. }
        | Command::Bindings
        | Command::Update { .. } => 2,
    }
}

fn open(context: &Context) -> Result<Service> {
    Service::open(&context.vault, &context.data_dir)
}

/// `project link --repo <path> --project <slug>`: record a local repository
/// mapping (never a Vault event). The slug comes from `--project` or
/// `AGENDAR_PROJECT`.
fn run_project_link(cli: &Cli, repo: &std::path::Path) -> i32 {
    let data_root = match crate::args::resolve_data_root(cli) {
        Ok(root) => root,
        Err(error) => return output::error(&error),
    };
    let local = match agendar_core::runtime::LocalPaths::new(&data_root) {
        Ok(local) => local,
        Err(error) => return output::error(&error),
    };
    // A link is local state. Separation is validated here, before any
    // directory, lock or config file is created.
    if let Err(error) = validate_link_paths(cli, &local) {
        return output::error(&error);
    }
    let project = match &cli.project {
        Some(value) => agendar_core::model::ProjectSlug::parse(value),
        None => match std::env::var("AGENDAR_PROJECT") {
            Ok(value) if !value.trim().is_empty() => {
                agendar_core::model::ProjectSlug::parse(&value)
            }
            _ => Err(
                AgendarError::new(ErrorCode::Usage, "Invalid argument: --project.")
                    .with_field("/project")
                    .with_detail("project_required")
                    .with_hint(format!(
                        "Example: {} project link --repo . --project demo",
                        agendar_core::service::cli_token()
                    )),
            ),
        },
    };
    let project = match project {
        Ok(project) => project,
        Err(error) => return output::error(&error),
    };
    if let Err(error) = agendar_core::runtime::ensure_dir(local.data_root()) {
        return output::error(&error);
    }
    match agendar_core::runtime::Config::map_repo(local.data_root(), repo, &project) {
        Ok(()) => {
            let canonical = repo
                .canonicalize()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| repo.to_string_lossy().into_owned());
            output::success(
                "project link",
                json!({"repo": canonical, "project": project}),
                None,
            )
        }
        Err(error) => output::error(&error),
    }
}

/// Reject a link whose data root equals, contains or is contained by the
/// selected Vault, and a data root that is itself an existing Vault. The Vault
/// is resolved from `--vault`, `AGENDAR_VAULT` or the local config, in that
/// order, and may not exist yet: linking is valid before `init`.
fn validate_link_paths(cli: &Cli, local: &agendar_core::runtime::LocalPaths) -> Result<()> {
    let data_root = local.data_root();
    let config = if data_root.exists() {
        agendar_core::runtime::Config::load_or_default(data_root)?
    } else {
        agendar_core::runtime::Config::default()
    };
    if let Some(vault) = crate::args::resolve_vault(cli, &config) {
        local.validate_against_vault_selection(&vault)?;
    }
    local.validate_not_a_vault()
}

fn read_json_input(value: &str) -> Result<Value> {
    let bytes = if value == "-" {
        read_stdin_bounded(MAX_JSON_INPUT_BYTES)?
    } else {
        fs::read(value).map_err(|error| {
            AgendarError::new(
                ErrorCode::Usage,
                format!("Invalid argument: --json. Cannot read the input file: {error}."),
            )
            .with_field("/json")
            .with_detail("json_input")
        })?
    };
    if bytes.len() > MAX_JSON_INPUT_BYTES {
        return Err(AgendarError::validation_at(
            "/json",
            "json_input_too_large",
            "The JSON input must be at most 64 KiB.",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        AgendarError::validation_at(
            "/json",
            "json_input_malformed",
            format!("The JSON input is not one valid object: {error}."),
        )
    })
}

fn read_stdin_bounded(limit: usize) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    std::io::stdin()
        .lock()
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut buffer)
        .map_err(AgendarError::from)?;
    Ok(buffer)
}

fn typed<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|error| {
        AgendarError::validation_at(
            "/json",
            "json_input_shape",
            format!("The JSON input has an unexpected shape: {error}."),
        )
    })
}

fn freshness(service: &Service, cached: bool) -> Option<Freshness> {
    service
        .refresh(SystemClock.now(), true)
        .ok()
        .and_then(|fresh| Service::freshness(&fresh, cached).ok())
}

fn finish(service: &Service, command: &str, data: Value, cached: bool) -> i32 {
    output::success(command, data, freshness(service, cached))
}

/// Add the session's debt line (`debt`) to a boundary command's result when
/// it owes anything: `open: 2 yours (1 past ETA), 1 free to take, 26 planned
/// · agendar settle`.
fn with_debt(
    service: &Service,
    project: &agendar_core::model::ProjectSlug,
    session: &agendar_core::model::SessionId,
    mut data: Value,
) -> Value {
    if let (Some(line), Some(object)) = (service.debt_line(project, session), data.as_object_mut())
    {
        object.insert("debt".into(), Value::from(line));
    }
    data
}

fn run_settle(context: &Context) -> i32 {
    let (project, session) = match (context.require_project(), context.require_session()) {
        (Ok(project), Ok(session)) => (project, session),
        (Err(error), _) | (_, Err(error)) => return output::error(&error),
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    match service.settle(&project, &session, context.cached) {
        Ok((data, shown)) => {
            let code = finish(&service, "settle", data, context.cached);
            // Shown candidates rest for a week, only once the agent saw them.
            if code == 0
                && let Err(error) = service.mark_settle_shown(&project, &shown)
                && context.verbose
            {
                eprintln!(
                    "agendar settle: shown cursor was not recorded: {}",
                    error.message()
                );
            }
            code
        }
        Err(error) => output::error(&error),
    }
}

fn run_init(
    context: &Context,
    harness: Option<crate::args::HarnessArg>,
    harness_session: Option<String>,
    pid: Option<u32>,
    repo: Option<PathBuf>,
) -> i32 {
    // Codex uses its durable thread id. Skill-only Claude Code has no hook to
    // supply an id, so the first init creates one and returns a reusable prefix.
    // An explicit --harness-session always wins.
    let harness_session = match (harness, harness_session) {
        (Some(crate::args::HarnessArg::Codex), None) => std::env::var("CODEX_THREAD_ID")
            .ok()
            .filter(|thread| !thread.trim().is_empty()),
        (Some(crate::args::HarnessArg::ClaudeCode), None) => Some(
            context
                .request_id
                .clone()
                .unwrap_or_else(|| ulid::Ulid::generate().to_string()),
        ),
        (_, explicit) => explicit,
    };
    // Skill-only Claude can discover the actual harness process instead of
    // accidentally registering a shell wrapper supplied through $PPID.
    let pid = match (harness, pid) {
        (Some(crate::args::HarnessArg::ClaudeCode), None) => {
            agendar_core::runtime::sessions::find_harness_process().map(|(pid, _)| pid)
        }
        (_, explicit) => explicit,
    };
    let registration_requested = harness.is_some() || harness_session.is_some() || pid.is_some();
    if registration_requested
        && harness == Some(crate::args::HarnessArg::ClaudeCode)
        && pid.is_none()
    {
        return output::error(
            &AgendarError::new(
                ErrorCode::Context,
                "Cannot find the running Claude Code process automatically.",
            )
            .with_detail("missing_harness_process")
            .with_hint(
                "Pass the running Claude Code process ID with --pid; do not use a shell wrapper.",
            ),
        );
    }
    if registration_requested
        && (harness.is_none() || harness_session.is_none() || pid.is_none() || repo.is_none())
    {
        let hint = if harness == Some(crate::args::HarnessArg::Codex) && harness_session.is_none() {
            "Pass --harness-session <thread-id>, or run inside a Codex session where CODEX_THREAD_ID is set; --pid and --repo are also required to register a session."
        } else {
            "Pass --harness, --harness-session, --pid and --repo together to register a session."
        };
        return output::error(
            &AgendarError::new(
                ErrorCode::Usage,
                "Invalid argument: --harness/--harness-session/--pid/--repo.",
            )
            .with_field("/harness")
            .with_detail("registration_flags")
            .with_hint(hint),
        );
    }
    // A repository link can supply the project when --repo is given without
    // --project, so `init --harness codex --harness-session <id> --pid <pid>
    // --repo <path>` works after one `project link`.
    let repo_project = match (&repo, &context.project, context.project_explicit) {
        (Some(_), Some(project), true) => Some(project.clone()),
        (Some(repo), fallback, _) => {
            let mapped = agendar_core::runtime::Config::load_or_default(&context.data_dir)
                .ok()
                .and_then(|config| config.project_for_cwd(repo));
            match mapped {
                Some(project) => Some(project),
                None => {
                    // Without a link, a cwd-derived project is only valid when
                    // the cwd is inside this same repository.
                    let inside = std::env::current_dir()
                        .ok()
                        .and_then(|cwd| {
                            let cwd = cwd.canonicalize().ok()?;
                            let repo = repo.canonicalize().ok()?;
                            Some(cwd.starts_with(&repo))
                        })
                        .unwrap_or(false);
                    if inside { fallback.clone() } else { None }
                }
            }
        }
        (None, project, _) => project.clone(),
    };
    if repo.is_some() && repo_project.is_none() {
        return output::error(
            &AgendarError::new(ErrorCode::Context, "Missing project for this repository.")
                .with_detail("repo_without_project")
                .with_hint(format!(
                    "Pass --project, or run `{} project link --repo <path> --project <slug>` first.",
                    agendar_core::service::cli_token()
                )),
        );
    }
    let (service, created) = match Service::init(&context.vault, &context.data_dir) {
        Ok(pair) => pair,
        Err(error) => return output::error(&error),
    };
    if let Err(error) = service.select_config(repo_project.as_ref()) {
        return output::error(&error);
    }
    if let Some(repo) = &repo
        && let Some(project) = &repo_project
        && let Err(error) = service.map_repo(repo, project)
    {
        return output::error(&error);
    }
    let mut session = None;
    let mut not_bound = None;
    if registration_requested {
        let project = match repo_project.clone() {
            Some(project) => project,
            None => {
                return output::error(&AgendarError::new(
                    ErrorCode::Context,
                    "Missing project for this session.",
                )
                .with_detail("missing_project")
                .with_hint(format!(
                    "Pass --project, or run `{} project link --repo <path> --project <slug>` first.",
                    agendar_core::service::cli_token()
                )))
            }
        };
        let harness = Harness::from(harness.expect("checked above"));
        let harness_session = harness_session.expect("checked above");
        let pid = pid.expect("checked above");
        let repo = repo.expect("checked above");
        let process_start = match process_identity(pid) {
            Some(start) => start,
            None => {
                return output::error(
                    &AgendarError::new(ErrorCode::Context, "The harness process was not found.")
                        .with_detail("missing_harness_process")
                        .with_hint("Use the running harness process id with --pid."),
                );
            }
        };
        let registration = SessionRegistration {
            project,
            harness,
            harness_session_id: harness_session,
            pid,
            process_start,
            repo_root: repo,
        };
        // `--session X`: the agent already has an Agendar session (its
        // harness session id changed, for example after `/clear`). Bind the
        // new id to X when X is this process's or its process is gone;
        // otherwise register as usual and say why X was not kept.
        if let Some(wanted) = &context.session {
            match agendar_core::runtime::steward::bind_session(&service, wanted, &registration) {
                Ok(Ok(())) => session = Some(wanted.clone()),
                Ok(Err(why)) => {
                    not_bound = Some(format!(
                        "Session {} was not kept: {why}. This is a new session.",
                        wanted.as_str()
                    ));
                }
                Err(error) => return output::error(&error),
            }
        }
        if session.is_none() {
            match register_session(&service, registration) {
                Ok(id) => session = Some(id),
                Err(error) => return output::error(&error),
            }
        }
    }
    let mut data = json!({
        "vault_id": service.vault().id(),
        "host": service.host(),
        "created": created,
    });
    if let Some(session) = session {
        if let Some(project) = repo_project.as_ref() {
            data["project"] = json!(project);
            data["context_prefix"] = json!(service.command_prefix(project, &session));
            data = with_debt(&service, project, &session, data);
        }
        data["session"] = json!(session);
    }
    if let Some(note) = not_bound {
        data["session_note"] = json!(note);
    }
    output::success("init", data, None)
}

fn run_project_declare(context: &Context, json: &str) -> i32 {
    let (project, session) = match (context.require_project(), context.require_session()) {
        (Ok(project), Ok(session)) => (project, session),
        (Err(error), _) | (_, Err(error)) => return output::error(&error),
    };
    let mut input = match read_json_input(json) {
        Ok(input) => input,
        Err(error) => return output::error(&error),
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    // An icon file is published to the Vault first, like option images.
    if let Err(error) = delivery::resolve_project_icon(&service, &project, &mut input) {
        return output::error(&error);
    }
    let patch: ProjectDeclared = match typed(input) {
        Ok(patch) => patch,
        Err(error) => return output::error(&error),
    };
    match service.declare_project(&project, &session, patch, context.request_id.as_deref()) {
        Ok(data) => finish(&service, "project declare", data, context.cached),
        Err(error) => output::error(&error),
    }
}

fn run_feature_declare(context: &Context, node: &str, json: &str) -> i32 {
    let (project, session) = match (context.require_project(), context.require_session()) {
        (Ok(project), Ok(session)) => (project, session),
        (Err(error), _) | (_, Err(error)) => return output::error(&error),
    };
    let node = match NodePath::parse(node) {
        Ok(node) => node,
        Err(error) => return output::error(&error),
    };
    let patch: PlanFeature = match read_json_input(json).and_then(typed) {
        Ok(patch) => patch,
        Err(error) => return output::error(&error),
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    match service.declare_feature(
        &project,
        &session,
        node,
        patch,
        context.request_id.as_deref(),
    ) {
        Ok(data) => finish(&service, "feature declare", data, context.cached),
        Err(error) => output::error(&error),
    }
}

fn run_progress(context: &Context, action: ProgressCommand) -> i32 {
    let (project, session) = match (context.require_project(), context.require_session()) {
        (Ok(project), Ok(session)) => (project, session),
        (Err(error), _) | (_, Err(error)) => return output::error(&error),
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    let boundary = !matches!(action, ProgressCommand::Started { .. });
    let result = match action {
        ProgressCommand::Started {
            node,
            eta,
            note,
            applies,
        } => {
            let node = NodePath::parse(&node);
            let eta = parse_eta(&eta);
            let applies: Result<Vec<EventId>> =
                applies.iter().map(|id| EventId::parse(id)).collect();
            match (node, eta, applies) {
                (Ok(node), Ok(eta), Ok(applies)) => service.progress_started_applying(
                    &project,
                    &session,
                    &node,
                    eta,
                    note,
                    &applies,
                    context.request_id.as_deref(),
                ),
                (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => Err(error),
            }
        }
        ProgressCommand::Done {
            node,
            attempt,
            note,
        } => progress_finished(
            &service,
            &project,
            &session,
            &node,
            &attempt,
            WorkStatus::Done,
            Vec::new(),
            note,
            context.request_id.as_deref(),
        ),
        ProgressCommand::Dropped {
            node,
            attempt: Some(attempt),
            note,
        } => progress_finished(
            &service,
            &project,
            &session,
            &node,
            &attempt,
            WorkStatus::Dropped,
            Vec::new(),
            note,
            context.request_id.as_deref(),
        ),
        ProgressCommand::Dropped {
            node,
            attempt: None,
            note,
        } => NodePath::parse(&node).and_then(|node| {
            service.drop_unstarted(
                &project,
                &session,
                &node,
                note,
                context.request_id.as_deref(),
            )
        }),
        ProgressCommand::Verified {
            node,
            attempt,
            json,
        } => match read_json_input(&json).and_then(typed::<VerifiedInput>) {
            Ok(input) => progress_finished(
                &service,
                &project,
                &session,
                &node,
                &attempt,
                WorkStatus::Verified,
                input.evidence,
                None,
                context.request_id.as_deref(),
            ),
            Err(error) => Err(error),
        },
    };
    match result {
        Ok(data) if boundary => {
            let data = with_debt(&service, &project, &session, data);
            finish(&service, "progress", data, context.cached)
        }
        Ok(data) => finish(&service, "progress", data, context.cached),
        Err(error) => output::error(&error),
    }
}

#[allow(clippy::too_many_arguments)]
fn progress_finished(
    service: &Service,
    project: &agendar_core::model::ProjectSlug,
    session: &agendar_core::model::SessionId,
    node: &str,
    attempt: &str,
    status: WorkStatus,
    evidence: Vec<agendar_core::model::CriterionResult>,
    note: Option<String>,
    request_id: Option<&str>,
) -> Result<Value> {
    let node = NodePath::parse(node)?;
    let attempt = EventId::parse(attempt)?;
    service.progress_finished(
        project, session, &node, &attempt, status, evidence, note, request_id,
    )
}

fn run_pulse(
    context: &Context,
    node: String,
    attempt: String,
    message: String,
    boundary: bool,
    eta: Option<String>,
) -> i32 {
    let (project, session) = match (context.require_project(), context.require_session()) {
        (Ok(project), Ok(session)) => (project, session),
        (Err(error), _) | (_, Err(error)) => return output::error(&error),
    };
    let node = NodePath::parse(&node);
    let attempt = EventId::parse(&attempt);
    let eta = match eta {
        Some(value) => match parse_eta(&value) {
            Ok(eta) => Ok(Some(eta)),
            Err(error) => Err(error),
        },
        None => Ok(None),
    };
    let (node, attempt, eta) = match (node, attempt, eta) {
        (Ok(node), Ok(attempt), Ok(eta)) => (node, attempt, eta),
        (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
            return output::error(&error);
        }
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    match service.pulse(
        &project,
        &session,
        &node,
        &attempt,
        message,
        boundary,
        eta,
        context.request_id.as_deref(),
    ) {
        Ok(data) => finish(&service, "pulse", data, context.cached),
        Err(error) => output::error(&error),
    }
}

/// The quick `ask` form: a question and its options, the rest defaulted.
pub struct QuickAsk {
    pub question: Option<String>,
    pub options: Vec<String>,
    pub recommend: Option<usize>,
    pub why: Option<String>,
    /// The takeover form: what the agent read of the other agent's work.
    pub takeover: Option<String>,
}

/// The card by which an agent asks the owner to let it continue a step
/// another agent holds. Choosing `takeover` admits its start.
fn takeover_card(node: Option<&str>, read: &str, why: Option<&str>) -> Result<Value> {
    let node = node.ok_or_else(|| {
        AgendarError::new(ErrorCode::Usage, "Name the step you want to continue.")
            .with_detail("ask_node_missing")
            .with_hint("agendar ask --node <step> --takeover \"<what you read of its work>\"")
    })?;
    let question = format!("May I continue {node}?");
    let question = if question.chars().count() > agendar_core::validate::limits::CARD_QUESTION {
        "May I continue this step?".to_owned()
    } else {
        question
    };
    Ok(json!({
        "why_now": why.unwrap_or("Another agent holds this step; I can continue it now."),
        "question": question,
        "reversibility": "easy",
        "reach": "local",
        "external": false,
        "domain": "code",
        "options": [
            {
                "id": agendar_core::model::TAKEOVER_OPTION,
                "label": "Let this agent continue",
                "consequence": "It takes the step from where the work stands; the agent that held it is told to stop.",
                "reversibility": "easy",
                "external": false,
            },
            {
                "id": "leave",
                "label": "Leave it with its agent",
                "consequence": "The step stays with the agent that started it; this one does other work.",
                "reversibility": "easy",
                "external": false,
            },
        ],
        "recommended": {
            "option": agendar_core::model::TAKEOVER_OPTION,
            "confidence": "low",
            "because": "The agent's request; your call.",
        },
        "already_checked": [{"text": read, "claim": "agent_says"}],
        "done_when": [{"id": "answered", "text": "The step goes on with the agent you chose."}],
        "on_timeout": {"kind": "hold"},
    }))
}

const QUICK_IDS: [&str; 4] = ["a", "b", "c", "d"];

/// Build a full card from the quick form: easy, local, held on timeout.
fn quick_card(quick: &QuickAsk) -> Result<Value> {
    let question = quick.question.as_deref().ok_or_else(|| {
        AgendarError::new(ErrorCode::Usage, "Give --json with a card, or --question with two to four --option.")
            .with_detail("ask_input_missing")
            .with_hint("agendar ask --question \"Which store?\" --option \"SQLite: one file\" --option \"JSON lines: diffable\"")
    })?;
    if !(2..=4).contains(&quick.options.len()) {
        return Err(AgendarError::validation_at(
            "/options",
            "quick_ask_options",
            "Give two to four --option values.",
        ));
    }
    let options: Vec<Value> = quick
        .options
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let (label, consequence) = text
                .split_once(": ")
                .or_else(|| text.split_once('：'))
                .map(|(label, rest)| (label.trim(), rest.trim()))
                .unwrap_or((text.trim(), text.trim()));
            json!({
                "id": QUICK_IDS[index],
                "label": label,
                "consequence": if consequence.is_empty() { label } else { consequence },
                "reversibility": "easy",
                "external": false,
            })
        })
        .collect();
    let recommended = quick.recommend.filter(|n| (1..=options.len()).contains(n));
    Ok(json!({
        "why_now": quick.why.as_deref().unwrap_or("Needed before the work goes on."),
        "question": question,
        "reversibility": "easy",
        "reach": "local",
        "external": false,
        "domain": "code",
        "options": options,
        "recommended": {
            "option": QUICK_IDS[recommended.unwrap_or(1) - 1],
            "confidence": if recommended.is_some() { "med" } else { "low" },
            "because": if recommended.is_some() { "The agent's pick; your call." } else { "No preference; your call." },
        },
        "already_checked": [],
        "done_when": [{"id": "answered", "text": "The work follows the chosen option."}],
        "on_timeout": {"kind": "hold"},
    }))
}

/// The step this session is doing, for an `ask` without `--node`.
fn current_step(
    service: &Service,
    project: &agendar_core::model::ProjectSlug,
    session: &agendar_core::model::SessionId,
) -> Result<NodePath> {
    let now = agendar_core::vault::writer::Clock::now(&agendar_core::vault::writer::SystemClock);
    let state = service.export_state(now)?;
    let doing: Vec<NodePath> = state
        .projects
        .get(project)
        .map(|project| {
            project
                .features
                .values()
                .filter(|feature| {
                    feature.progress == agendar_core::model::Progress::Doing
                        && feature.session.as_ref() == Some(session)
                })
                .map(|feature| feature.node.clone())
                .collect()
        })
        .unwrap_or_default();
    match doing.as_slice() {
        [node] => Ok(node.clone()),
        [] => Err(
            AgendarError::new(ErrorCode::Usage, "Name the step this question is about.")
                .with_detail("ask_node_missing")
                .with_hint("Pass --node <step>, or start the step first."),
        ),
        _ => Err(AgendarError::new(
            ErrorCode::Usage,
            "You are doing several steps; name the one this question is about.",
        )
        .with_detail("ask_node_ambiguous")
        .with_hint("Pass --node <step>.")),
    }
}

fn run_ask(
    context: &Context,
    node: Option<&str>,
    json: Option<&str>,
    quick: QuickAsk,
    mode: agendar_core::model::Mode,
) -> i32 {
    let (project, session) = match (context.require_project(), context.require_session()) {
        (Ok(project), Ok(session)) => (project, session),
        (Err(error), _) | (_, Err(error)) => return output::error(&error),
    };
    let mut input = match json {
        Some(json) => match read_json_input(json) {
            Ok(input) => input,
            Err(error) => return output::error(&error),
        },
        None => {
            let card = match quick.takeover.as_deref() {
                Some(read) => takeover_card(node, read, quick.why.as_deref()),
                None => quick_card(&quick),
            };
            match card {
                Ok(input) => input,
                Err(error) => return output::error(&error),
            }
        }
    };
    let node = match node {
        Some(node) => match NodePath::parse(node) {
            Ok(node) => node,
            Err(error) => return output::error(&error),
        },
        None => {
            let service = match open(context) {
                Ok(service) => service,
                Err(error) => return output::error(&error),
            };
            match current_step(&service, &project, &session) {
                Ok(node) => node,
                Err(error) => return output::error(&error),
            }
        }
    };
    // Validate the rest of the card before publishing any option image.
    if let Err(error) = typed::<DecisionCard>(delivery::without_option_images(&input))
        .and_then(|card| agendar_core::model::validate_card_input(&card))
    {
        return output::error(&error);
    }
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    if let Err(error) = delivery::resolve_option_images(&service, &project, &mut input) {
        return output::error(&error);
    }
    let card: DecisionCard = match typed(input) {
        Ok(card) => card,
        Err(error) => return output::error(&error),
    };
    match service.ask(
        &project,
        &session,
        &node,
        card,
        mode,
        context.request_id.as_deref(),
    ) {
        Ok(result) => match serde_json::to_value(&result) {
            Ok(data) => finish(&service, "ask", data, context.cached),
            Err(error) => output::error(&AgendarError::new(
                ErrorCode::Io,
                format!("Cannot serialize the ask result: {error}."),
            )),
        },
        Err(error) => output::error(&error),
    }
}

fn run_inbox(context: &Context, node: Option<String>) -> i32 {
    let (project, session) = match (context.require_project(), context.require_session()) {
        (Ok(project), Ok(session)) => (project, session),
        (Err(error), _) | (_, Err(error)) => return output::error(&error),
    };
    let node = match node {
        Some(node) => match NodePath::parse(&node) {
            Ok(node) => Some(node),
            Err(error) => return output::error(&error),
        },
        None => None,
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    match service.inbox(&project, &session, node.as_ref(), context.cached) {
        Ok((packets, rules, _)) => {
            let rules: Vec<Value> = rules.iter().map(|record| json!(record.rule)).collect();
            // The inbox refresh just ingested; read the nudges from that index.
            // Human plan edits come first, each shown to this session once.
            let (plan, plan_seen) = service
                .plan_nudges(&project, &session, true)
                .unwrap_or_default();
            let nudges: Vec<_> = plan
                .into_iter()
                .chain(service.nudges(&project, &session, true).unwrap_or_default())
                .filter(|nudge| {
                    node.as_ref()
                        .is_none_or(|node| nudge.node.as_ref().is_none_or(|own| own == node))
                })
                .collect();
            let data = with_debt(
                &service,
                &project,
                &session,
                json!({
                    "packets": packets,
                    "rules": rules,
                    "session": session,
                    "nudges": nudges,
                }),
            );
            let fresh = freshness(&service, context.cached);
            let code = output::success("inbox", data, fresh);
            if code == 0
                && let Err(error) = service.mark_delivered_via(
                    &project,
                    &session,
                    &packets,
                    DeliveredOutcome::InboxRead,
                    "inbox",
                )
                && context.verbose
            {
                eprintln!(
                    "agendar inbox: delivery checkpoint was not recorded: {}",
                    error.message()
                );
            }
            // Plan edits are shown once: move this session's cursor past them,
            // only after the output reached the agent.
            if code == 0
                && let Some(seen) = &plan_seen
                && let Err(error) = service.mark_plan_seen(&session, seen)
                && context.verbose
            {
                eprintln!(
                    "agendar inbox: plan cursor was not recorded: {}",
                    error.message()
                );
            }
            code
        }
        Err(error) => output::error(&error),
    }
}

fn run_wait(
    context: &Context,
    ask_id: Option<&str>,
    delivery: Option<&str>,
    timeout: Option<String>,
) -> i32 {
    let (project, session) = match (context.require_project(), context.require_session()) {
        (Ok(project), Ok(session)) => (project, session),
        (Err(error), _) | (_, Err(error)) => return output::error(&error),
    };
    let target = match (ask_id, delivery) {
        (_, Some(delivery)) => EventId::parse(delivery).map(WaitTarget::Verdict),
        (Some(ask), None) => EventId::parse(ask).map(WaitTarget::Answer),
        (None, None) => Err(AgendarError::validation_at(
            "/ask_id",
            "wait_target_missing",
            "Name an ask id, or --delivery <id>.",
        )),
    };
    let target = match target {
        Ok(target) => target,
        Err(error) => return output::error(&error),
    };
    let timeout = match timeout {
        Some(value) => match parse_duration(&value) {
            Ok(timeout) => Some(timeout),
            Err(error) => return output::error(&error),
        },
        None => None,
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    let cancel = Arc::new(AtomicBool::new(false));
    {
        use std::sync::atomic::Ordering;
        let cancel = cancel.clone();
        let _ = ctrlc::set_handler(move || {
            cancel.store(true, Ordering::SeqCst);
        });
    }
    if context.verbose {
        match &target {
            WaitTarget::Answer(ask) => {
                eprintln!("agendar wait: waiting for a current answer to {ask}.")
            }
            WaitTarget::Verdict(delivery) => {
                eprintln!("agendar wait: waiting for the verdict on delivery {delivery}.")
            }
        }
    }
    let options = WaitOptions {
        timeout,
        cancel: Some(cancel),
        ..WaitOptions::default()
    };
    let mut observer = agendar_core::service::NoopWaitObserver;
    match service.wait_for(&project, &session, &target, &options, &mut observer) {
        Ok(packet) => {
            let fresh = freshness(&service, false);
            let data = match &target {
                WaitTarget::Answer(ask) => json!({"ask_id": ask, "packet": packet}),
                WaitTarget::Verdict(delivery) => {
                    json!({"delivery_id": delivery, "packet": packet})
                }
            };
            let code = output::success("wait", data, fresh);
            // A verdict has no ack: printing it is this session's one sighting,
            // so a later Stop or rewake does not wake for it again.
            if matches!(target, WaitTarget::Verdict(_))
                && let Err(error) = service.mark_delivered_via(
                    &project,
                    &session,
                    std::slice::from_ref(&packet),
                    DeliveredOutcome::Accepted,
                    "wait",
                )
                && context.verbose
            {
                eprintln!(
                    "agendar wait: the sighting was not recorded: {}",
                    error.message()
                );
            }
            code
        }
        Err(error) => output::error(&error),
    }
}

fn run_ack(context: &Context, answer_id: &str, restarted: Vec<String>) -> i32 {
    let (project, session) = match (context.require_project(), context.require_session()) {
        (Ok(project), Ok(session)) => (project, session),
        (Err(error), _) | (_, Err(error)) => return output::error(&error),
    };
    let answer = match EventId::parse(answer_id) {
        Ok(answer) => answer,
        Err(error) => return output::error(&error),
    };
    let mut restarted_nodes = Vec::new();
    for node in restarted {
        match NodePath::parse(&node) {
            Ok(node) => restarted_nodes.push(node),
            Err(error) => return output::error(&error),
        }
    }
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    match service.ack(
        &project,
        &session,
        &answer,
        restarted_nodes,
        context.request_id.as_deref(),
    ) {
        Ok(data) => finish(&service, "ack", data, context.cached),
        Err(error) => output::error(&error),
    }
}

fn run_withdraw(context: &Context, ask_id: &str, reason: String) -> i32 {
    let (project, session) = match (context.require_project(), context.require_session()) {
        (Ok(project), Ok(session)) => (project, session),
        (Err(error), _) | (_, Err(error)) => return output::error(&error),
    };
    let ask = match EventId::parse(ask_id) {
        Ok(ask) => ask,
        Err(error) => return output::error(&error),
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    match service.withdraw(
        &project,
        &session,
        &ask,
        reason,
        context.request_id.as_deref(),
    ) {
        Ok(data) => finish(&service, "withdraw", data, context.cached),
        Err(error) => output::error(&error),
    }
}

fn run_blob_put(context: &Context, file: &std::path::Path, ext: &str) -> i32 {
    let project = match context.require_project() {
        Ok(project) => project,
        Err(error) => return output::error(&error),
    };
    if let Err(error) = context.require_session() {
        return output::error(&error);
    }
    let ext = match parse_blob_ext(ext) {
        Ok(ext) => ext,
        Err(error) => return output::error(&error),
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    let input = BlobInput {
        file: Some(file.to_string_lossy().into_owned()),
        ext: Some(ext),
        blob: None,
        alt: None,
        check: None,
    };
    const ATTACHMENTS: [BlobExt; 6] = [
        BlobExt::Txt,
        BlobExt::Md,
        BlobExt::Html,
        BlobExt::Png,
        BlobExt::Jpg,
        BlobExt::Webp,
    ];
    match delivery::upload_blob(&service, &project, &input, "/file", &ATTACHMENTS) {
        Ok(blob) => finish(
            &service,
            "blob put",
            delivery::blob_put_result(&blob),
            context.cached,
        ),
        Err(error) => output::error(&error),
    }
}

fn run_deliver(
    context: &Context,
    node: &str,
    attempt: &str,
    receipt: Option<&str>,
    json: &str,
    supersedes: Option<&str>,
) -> i32 {
    let (project, session) = match (context.require_project(), context.require_session()) {
        (Ok(project), Ok(session)) => (project, session),
        (Err(error), _) | (_, Err(error)) => return output::error(&error),
    };
    let node = match NodePath::parse(node) {
        Ok(node) => node,
        Err(error) => return output::error(&error),
    };
    let attempt = match EventId::parse(attempt) {
        Ok(attempt) => attempt,
        Err(error) => return output::error(&error),
    };
    let receipt = match receipt.map(EventId::parse).transpose() {
        Ok(receipt) => receipt,
        Err(error) => return output::error(&error),
    };
    let supersedes = match supersedes.map(EventId::parse).transpose() {
        Ok(supersedes) => supersedes,
        Err(error) => return output::error(&error),
    };
    let input: DeliveryInput = match read_json_input(json).and_then(typed) {
        Ok(input) => input,
        Err(error) => return output::error(&error),
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    let registration = match service.require_session(&session, &project) {
        Ok(registration) => registration,
        Err(error) => return output::error(&error),
    };
    // All local files are published before the event; a failure leaves no
    // delivery behind. Proposals are validated here and stored after append.
    let criteria_ids: Vec<CriterionId> = input
        .criteria
        .iter()
        .map(|criterion| criterion.criterion.clone())
        .collect();
    if let Err(error) = service.validate_delivery_target(
        &project,
        &node,
        &attempt,
        receipt.as_ref(),
        &criteria_ids,
        supersedes.as_ref(),
        &input.replies,
    ) {
        return output::error(&error);
    }
    let body =
        match delivery::build_body(&service, &project, &session, registration.harness, &input) {
            Ok((body, _uploaded)) => body,
            Err(error) => return output::error(&error),
        };
    if let Err(error) = delivery::validate_proposals(&service, &project, None, &input) {
        return output::error(&error);
    }
    match service.deliver(
        &project,
        &session,
        &node,
        &attempt,
        receipt.as_ref(),
        supersedes.as_ref(),
        body,
        context.request_id.as_deref(),
    ) {
        Ok(data) => {
            if let Some(event_id) = data
                .get("event_id")
                .and_then(Value::as_str)
                .and_then(|value| EventId::parse(value).ok())
                && let Err(error) =
                    delivery::validate_proposals(&service, &project, Some(&event_id), &input)
            {
                return output::error(&error);
            }
            let data = with_debt(&service, &project, &session, data);
            finish(&service, "deliver", data, context.cached)
        }
        Err(error) => output::error(&error),
    }
}

fn run_rules(context: &Context) -> i32 {
    let project = match context.require_project() {
        Ok(project) => project,
        Err(error) => return output::error(&error),
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    match service.rules(&project, context.cached) {
        Ok(rules) => {
            let rules: Vec<Value> = rules
                .iter()
                .map(|record| {
                    json!({
                        "rule": record.rule,
                        "event_id": record.event_id,
                    })
                })
                .collect();
            finish(&service, "rules", json!({"rules": rules}), context.cached)
        }
        Err(error) => output::error(&error),
    }
}

fn run_board(context: &Context, view: &BoardView, out: Option<&std::path::Path>) -> i32 {
    let project = match context.require_project() {
        Ok(project) => project,
        Err(error) => return output::error(&error),
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    let board = match service.board(&project, context.session.as_ref(), context.cached, view) {
        Ok(board) => board,
        Err(error) => return output::error(&error),
    };
    match out {
        None => finish(&service, "board", board, context.cached),
        Some(out) => match crate::board::write_out(&context.vault, out, &board) {
            Ok(saved) => finish(&service, "board", saved, context.cached),
            Err(error) => output::error(&error),
        },
    }
}

fn run_status(context: &Context) -> i32 {
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    match service.status(context.cached) {
        Ok(mut data) => {
            // The cached update check only: `status` never reaches the network.
            if let Value::Object(object) = &mut data {
                object.insert(
                    "update".to_owned(),
                    json!(agendar_core::update::cached_status(
                        &context.data_dir,
                        false
                    )),
                );
            }
            finish(&service, "status", data, context.cached)
        }
        Err(error) => output::error(&error),
    }
}

/// `prune`: the usage and reclaimable listing. It only lists; there is no
/// delete. One project when `--project` (or `AGENDAR_PROJECT`) names it, every
/// project otherwise: the repository link of the working directory does not
/// narrow it.
fn run_prune(context: &Context) -> i32 {
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    let project = context
        .project
        .as_ref()
        .filter(|_| context.project_explicit);
    let result = service
        .refresh(SystemClock.now(), context.cached)
        .and_then(|fresh| {
            let usage = service.storage_usage(&fresh, project)?;
            Ok((usage, Service::freshness(&fresh, context.cached)?))
        });
    match result {
        Ok((usage, freshness)) => output::success("prune", json!(usage), Some(freshness)),
        Err(error) => output::error(&error),
    }
}

/// `update [--check]`: one network check against the latest public release,
/// cached in the local data directory.
fn run_update(cli: &Cli) -> i32 {
    use agendar_core::update;
    let data_root = match crate::args::resolve_data_root(cli) {
        Ok(root) => root,
        Err(error) => return output::error(&error),
    };
    let url = match update::endpoint() {
        Ok(url) => url,
        Err(error) => return output::error(&error),
    };
    match update::check(&data_root, &update::HttpFetcher, &url, SystemClock.now()) {
        Ok(cache) => output::success(
            "update",
            json!(update::status(
                &cache,
                update::Channel::of_build(),
                update::current_version(),
                true,
            )),
            None,
        ),
        Err(error) => output::error(&error),
    }
}

fn run_rebuild(context: &Context) -> i32 {
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    match service.rebuild_index() {
        Ok(data) => finish(&service, "index rebuild", data, true),
        Err(error) => output::error(&error),
    }
}

fn run_state_export(context: &Context, as_of: &str) -> i32 {
    let as_of = match UtcTime::parse(as_of) {
        Ok(as_of) => as_of,
        Err(error) => return output::error(&error),
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    match service.export_state(as_of) {
        Ok(state) => match serde_json::to_value(&state) {
            Ok(value) => output::raw_canonical(&value),
            Err(error) => output::error(&AgendarError::new(
                ErrorCode::Io,
                format!("Cannot serialize the canonical state: {error}."),
            )),
        },
        Err(error) => output::error(&error),
    }
}

const SKILL_BYTES: &str = include_str!("../../../skills/agendar/SKILL.md");

fn run_skill(action: &SkillCommand) -> i32 {
    match action {
        SkillCommand::Print => {
            print!("{SKILL_BYTES}");
            use std::io::Write;
            let _ = std::io::stdout().flush();
            0
        }
        SkillCommand::Info => {
            let digest = Sha256::from_bytes(SKILL_BYTES.as_bytes());
            output::success(
                "skill info",
                json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "sha256": digest,
                }),
                None,
            )
        }
    }
}

fn run_steward(context: &Context, once: bool) -> i32 {
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    if once {
        return match steward_tick(&service) {
            Ok(report) => finish(
                &service,
                "steward",
                json!({
                    "processed": report.processed,
                    "delivered": report.delivered,
                    "lost": report.lost,
                    "resumable": report.resumable,
                    "queued": report.queued,
                    "capabilities": report.capabilities,
                }),
                false,
            ),
            Err(error) => output::error(&error),
        };
    }
    // Long-running steward: exactly one startup JSON line, then stderr logs
    // only until a termination signal. The app and the CLI share this tick.
    let capabilities: Vec<_> = [Harness::ClaudeCode, Harness::Codex, Harness::Generic]
        .into_iter()
        .map(|harness| service.capabilities_for(harness))
        .collect();
    let code = output::success(
        "steward",
        json!({
            "processed": 0,
            "delivered": 0,
            "lost": 0,
            "resumable": 0,
            "capabilities": capabilities,
        }),
        None,
    );
    if code != 0 {
        return code;
    }
    let stop = Arc::new(AtomicBool::new(false));
    {
        use std::sync::atomic::Ordering;
        let stop = stop.clone();
        let _ = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst));
    }
    while !stop.load(std::sync::atomic::Ordering::SeqCst) {
        match steward_tick(&service) {
            Ok(report) if context.verbose => eprintln!(
                "agendar steward: processed {} delivered {} lost {} resumable {}.",
                report.processed, report.delivered, report.lost, report.resumable
            ),
            Ok(_) => {}
            Err(error) => eprintln!(
                "agendar steward: tick failed: {} ({})",
                error.message(),
                error.code().as_str()
            ),
        }
        // Stay responsive to a termination signal without a second poll loop.
        for _ in 0..50 {
            if stop.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    0
}

fn hook_error(error: &AgendarError) -> i32 {
    // Hooks are installed for every repository on the machine. In one that is
    // not linked to a project the session is not Agendar's: say nothing.
    if error.detail() == Some("unlinked_repository") {
        return 0;
    }
    eprintln!(
        "agendar hook: {} ({})",
        error.message(),
        error.code().as_str()
    );
    // The recovery hint is plain stderr text: a failed shim still emits no
    // harness JSON, but the operator must still see e.g. the `project link`
    // recovery step.
    if let Some(hint) = error.hint() {
        eprintln!("agendar hook: {hint}");
    }
    output::exit_code(error.code())
}

fn install_context() -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let cli = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|error| {
            AgendarError::new(
                ErrorCode::Io,
                format!("Cannot resolve the agendar executable: {error}."),
            )
            .with_detail("install_cli_path")
        })?;
    Ok((cli, agendar_core::runtime::install::home_dir()?))
}

/// What the harness still needs after a bind, in one line.
fn bind_next_step(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "Restart Claude Code (or open /hooks) so the Agendar hooks load.",
        Harness::Codex => {
            "Open /hooks in Codex and trust the Agendar hooks; Codex runs them only once trusted."
        }
        _ => "Restart the agent so it loads the agendar MCP server, then call its `board` tool.",
    }
}

fn run_bind(harness: Option<Harness>, bind: bool) -> i32 {
    let (cli, home) = match install_context() {
        Ok(context) => context,
        Err(error) => return output::error(&error),
    };
    let Some(harness) = harness.or_else(agendar_core::runtime::install::detect_harness) else {
        return output::error(
            &AgendarError::new(
                ErrorCode::Usage,
                "Could not tell which agent harness this shell runs in.",
            )
            .with_detail("bind_harness_unknown")
            .with_hint("Pass --harness claude_code|codex|cursor|gemini|agy|opencode|pi|dsh."),
        );
    };
    let result = if bind {
        agendar_core::runtime::install::bind(harness, &cli, &home)
    } else {
        agendar_core::runtime::install::unbind(harness, &cli, &home)
    };
    match result {
        Ok(binding) => output::success(
            if bind { "bind" } else { "unbind" },
            json!({
                "binding": binding,
                "next": if bind { bind_next_step(harness) } else { "Restart the agent so the change takes effect." },
            }),
            None,
        ),
        Err(error) => output::error(&error),
    }
}

fn run_bindings() -> i32 {
    let (cli, home) = match install_context() {
        Ok(context) => context,
        Err(error) => return output::error(&error),
    };
    output::success(
        "bindings",
        json!({
            "bindings": agendar_core::runtime::install::bindings(&cli, &home),
            "detected": agendar_core::runtime::install::detect_harness(),
        }),
        None,
    )
}

fn run_install_mcp(harness: Harness, yes: bool) -> i32 {
    let cli = match std::env::current_exe().and_then(|path| path.canonicalize()) {
        Ok(path) => path,
        Err(error) => {
            return output::error(
                &AgendarError::new(
                    ErrorCode::Io,
                    format!("Cannot resolve the agendar executable: {error}."),
                )
                .with_detail("install_cli_path"),
            );
        }
    };
    let home = match agendar_core::runtime::install::home_dir() {
        Ok(home) => home,
        Err(error) => return output::error(&error),
    };
    let plan = match agendar_core::runtime::install::plan_mcp(harness, &cli, &home) {
        Ok(plan) => plan,
        Err(error) => return output::error(&error),
    };
    let result = if yes {
        agendar_core::runtime::install::install_mcp(harness, &cli, &home, &plan.plan_digest)
    } else {
        Ok(plan)
    };
    match result {
        Ok(plan) => output::success("install-mcp", json!({"plan": plan, "installed": yes}), None),
        Err(error) => output::error(&error),
    }
}

fn run_install_hooks(
    harness: crate::args::HarnessArg,
    yes: bool,
    settings: Option<PathBuf>,
    dry_run: bool,
) -> i32 {
    if yes && dry_run {
        return output::error(
            &AgendarError::new(ErrorCode::Usage, "--yes and --dry-run cannot be combined.")
                .with_detail("install_hooks_arguments"),
        );
    }
    let harness: Harness = harness.into();
    let set = match harness {
        Harness::ClaudeCode => agendar_core::runtime::install::HookSet::Skill,
        Harness::Codex => agendar_core::runtime::install::HookSet::Codex,
        _ => {
            return output::error(
                &AgendarError::new(
                    ErrorCode::Unsupported,
                    "This harness has no supported hook installer yet.",
                )
                .with_detail("hook_harness_unsupported")
                .with_hint("Use --harness claude_code or --harness codex."),
            );
        }
    };
    let cli = match std::env::current_exe().and_then(|path| path.canonicalize()) {
        Ok(path) => path,
        Err(error) => {
            return output::error(
                &AgendarError::new(
                    ErrorCode::Io,
                    format!("Cannot resolve the agendar executable: {error}."),
                )
                .with_detail("install_cli_path"),
            );
        }
    };
    let home = match agendar_core::runtime::install::home_dir() {
        Ok(home) => home,
        Err(error) => return output::error(&error),
    };
    let plan = match agendar_core::runtime::install::plan_claude_hook_set(
        set,
        &cli,
        settings.as_deref(),
        &home,
    ) {
        Ok(plan) => plan,
        Err(error) => return output::error(&error),
    };
    let result = if yes && !dry_run {
        agendar_core::runtime::install::install_claude_hook_set(
            set,
            &cli,
            settings.as_deref(),
            &home,
            &plan.plan_digest,
        )
    } else {
        Ok(plan)
    };
    match result {
        Ok(plan) => output::success(
            "install-hooks",
            json!({"plan": plan, "installed": yes && !dry_run}),
            None,
        ),
        Err(error) => output::error(&error),
    }
}

/// `hook observe`: record a Claude Code hook event in the local presence file.
/// Every error is swallowed and nothing is printed: watching must never change
/// what the agent sees or does.
fn run_observe(data_dir: Option<&std::path::Path>) -> i32 {
    let observe = || -> Option<()> {
        let bytes = read_stdin_bounded(crate::hooks::MAX_STDIN_BYTES).ok()?;
        let input: Value = serde_json::from_slice(&bytes).ok()?;
        // Cursor's agent CLI also runs Claude Code's hooks; not a Claude session.
        if input.get("cursor_version").is_some() || std::env::var_os("CURSOR_VERSION").is_some() {
            return None;
        }
        let event = input.get("hook_event_name")?.as_str()?;
        let notification = input.get("notification_type").and_then(Value::as_str);
        let observation = agendar_core::runtime::presence::claude_observation(event, notification)?;
        let session = input.get("session_id")?.as_str()?;
        let cwd = input.get("cwd")?.as_str()?;
        let root = match data_dir {
            Some(root) => root.to_path_buf(),
            None => match std::env::var_os("AGENDAR_DATA_DIR") {
                Some(root) => std::path::PathBuf::from(root),
                None => agendar_core::runtime::LocalPaths::default_data_root().ok()?,
            },
        };
        let pid = agendar_core::runtime::sessions::find_harness_process().map(|(pid, _)| pid);
        agendar_core::runtime::presence::record(
            &root,
            &format!("claude_code:{session}"),
            "claude_code",
            std::path::Path::new(cwd),
            pid,
            observation,
            agendar_core::vault::writer::Clock::now(&agendar_core::vault::writer::SystemClock),
        )
        .ok()
    };
    let _ = observe();
    0
}

fn run_report(
    context: &Context,
    title: String,
    summary: String,
    file: &str,
    node: Option<&str>,
    kind: Option<agendar_core::model::ReportKind>,
    domain: Option<agendar_core::model::Domain>,
) -> i32 {
    let (project, session) = match (context.require_project(), context.require_session()) {
        (Ok(project), Ok(session)) => (project, session),
        (Err(error), _) | (_, Err(error)) => return output::error(&error),
    };
    let node = match node.map(NodePath::parse).transpose() {
        Ok(node) => node,
        Err(error) => return output::error(&error),
    };
    let ext = std::path::Path::new(file)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let ext = match ext.as_str() {
        "htm" => "html".to_owned(),
        "markdown" => "md".to_owned(),
        other => other.to_owned(),
    };
    let ext = match delivery::parse_blob_ext(&ext) {
        Ok(ext) => ext,
        Err(error) => return output::error(&error),
    };
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return output::error(&error),
    };
    let input = BlobInput {
        file: Some(file.to_owned()),
        ext: Some(ext),
        blob: None,
        alt: None,
        check: None,
    };
    let report = match delivery::upload_blob(
        &service,
        &project,
        &input,
        "/file",
        &[BlobExt::Html, BlobExt::Md, BlobExt::Txt],
    ) {
        Ok(report) => report,
        Err(error) => return output::error(&error),
    };
    match service.publish_report(
        &project,
        &session,
        node.as_ref(),
        agendar_core::service::ReportDraft {
            title,
            summary,
            report,
            kind,
            domain,
        },
        context.request_id.as_deref(),
    ) {
        Ok(data) => finish(&service, "report", data, context.cached),
        Err(error) => output::error(&error),
    }
}

fn run_hook(context: &Context, action: HookCommand) -> i32 {
    let shim_started = std::time::Instant::now();
    let service = match open(context) {
        Ok(service) => service,
        Err(error) => return hook_error(&error),
    };
    let bytes = match read_stdin_bounded(crate::hooks::MAX_STDIN_BYTES) {
        Ok(bytes) => bytes,
        Err(error) => return hook_error(&error),
    };
    if bytes.len() > crate::hooks::MAX_STDIN_BYTES {
        return hook_error(&AgendarError::validation_at(
            "/hook",
            "hook_input_too_large",
            "Hook stdin must be at most 1 MiB.",
        ));
    }
    let input: HookInput = match serde_json::from_slice(&bytes) {
        Ok(input) => input,
        Err(error) => {
            return hook_error(&AgendarError::validation_at(
                "/hook",
                "hook_input_malformed",
                format!("Hook stdin is not valid JSON: {error}."),
            ));
        }
    };
    if matches!(action, HookCommand::Rewake) {
        let explicit_project = context
            .project
            .as_ref()
            .filter(|_| context.project_explicit);
        return crate::hooks::rewake(&service, explicit_project, &input);
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    // Only an explicit flag/environment project overrides the harness cwd link:
    // the stdin cwd is the session's real repository, so a project inferred from
    // this process's own cwd is not authoritative for the hook.
    let explicit_project = context
        .project
        .as_ref()
        .filter(|_| context.project_explicit);
    match crate::hooks::run(
        &action,
        &service,
        explicit_project,
        &input,
        &mut lock,
        shim_started,
    ) {
        Ok(()) => 0,
        // No harness JSON after a failed shim; report a degraded diagnostic on
        // stderr only.
        Err(error) => hook_error(&error),
    }
}

/// Expose the harness capability description to tests (not part of JSON output).
pub fn capability_line(harness: Harness) -> String {
    let capability = match harness {
        Harness::ClaudeCode => agendar_core::adapters::ClaudeAdapter::default().capabilities(),
        _ => agendar_core::adapters::GenericAdapter::default().capabilities(),
    };
    format!("{:?} {}", capability.harness, capability.detail)
}
