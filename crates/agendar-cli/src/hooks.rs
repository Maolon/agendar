//! Internal Claude Code hook protocol (`agendar hook ...`).
//!
//! This is not the agent-facing command API. It reads harness JSON from stdin,
//! resolves the project/session from local registrations (never from arbitrary
//! stdin commands), writes harness JSON on stdout and, only after a successful
//! flush, records the delivery attempt (and which hook wrote it). It never
//! acknowledges a decision for an agent, and a recorded sighting is not an
//! ack: an answer is delivered when the agent acks it. Until then the Stop
//! hook holds for it once and every UserPromptSubmit names it.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;

use agendar_core::error::{AgendarError, ErrorCode, Result};
use agendar_core::model::{DeliveredOutcome, Harness, InboxPacket, ProjectSlug, SessionId};
use agendar_core::runtime::sessions::process_identity;
use agendar_core::runtime::steward::{
    SessionRegistration, continue_process_session, register_session,
};
use agendar_core::service::{LoopStatus, NoopWaitObserver, Service, WaitOptions, WaitingAnswer};

use crate::args::HookCommand;

/// The harness running this hook process (`agendar hook --harness`); one hook
/// process serves one event, so it is set once before dispatch.
static HOOK_HARNESS: std::sync::OnceLock<Harness> = std::sync::OnceLock::new();

/// Record which harness runs this hook process.
pub fn set_harness(harness: Harness) {
    let _ = HOOK_HARNESS.set(harness);
}

fn hook_harness() -> Harness {
    HOOK_HARNESS.get().copied().unwrap_or(Harness::ClaudeCode)
}

/// Maximum hook stdin payload.
pub const MAX_STDIN_BYTES: usize = 1_048_576;

/// Default total shim budget inside the installed 60 s hook timeout.
pub const DEFAULT_BUDGET_MS: u64 = 45_000;

/// Time reserved for serialization and flush at the end of the shim budget.
pub const OUTPUT_RESERVE: Duration = Duration::from_secs(2);

/// Minimal harness stdin shape. Unrelated recorded fields are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct HookInput {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub hook_event_name: Option<String>,
    #[serde(default)]
    pub stop_hook_active: Option<bool>,
    /// SessionStart: `startup`, `resume`, `clear`, `compact` or `fork`
    /// (Claude Code hooks reference, "SessionStart").
    #[serde(default)]
    pub source: Option<String>,
    /// Set by Cursor's agent CLI, which also runs Claude Code's hooks.
    #[serde(default)]
    pub cursor_version: Option<String>,
    /// Stop: the agent's final message (Claude Code and Codex send it).
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    /// Set when the hook fires inside a subagent or an in-process teammate
    /// (Claude Code hooks reference, "Common input fields"): its tool calls
    /// run the main conversation's hooks, under the same session.
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// Whether a final message ends by asking the human something. A question in
/// chat is easy to miss; Agendar is where questions reach the human.
pub fn asks_human(message: &str) -> bool {
    let last = message
        .trim()
        .rsplit("\n\n")
        .next()
        .unwrap_or_default()
        .trim();
    if last.is_empty() || last.starts_with("```") {
        return false;
    }
    if last.ends_with('?') || last.ends_with('？') {
        return true;
    }
    let lower = last.to_lowercase();
    [
        "do you want",
        "should i ",
        "shall i ",
        "would you like",
        "want me to",
        "let me know",
        "your call",
        "要不要",
        "是否要",
        "要我",
        "请确认",
        "請確認",
        "告诉我",
        "告訴我",
        "可以吗",
        "可以嗎",
        "好吗",
        "好嗎",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
}

/// A Claude Code hook running inside Cursor's agent CLI (which loads Claude
/// Code's settings too) must stay silent: that session belongs to Cursor.
pub fn foreign_host(input: &HookInput) -> bool {
    hook_harness() == Harness::ClaudeCode
        && (input.cursor_version.is_some() || std::env::var_os("CURSOR_VERSION").is_some())
}

impl HookInput {
    /// The harness session id, or a typed context error.
    pub fn harness_session(&self) -> Result<&str> {
        self.session_id.as_deref().ok_or_else(|| {
            AgendarError::new(
                ErrorCode::Context,
                "The hook input has no harness session id.",
            )
            .with_detail("hook_missing_session")
            .with_hint("Run the hook from a Claude Code session.")
        })
    }

    /// The harness working directory, or a typed context error.
    pub fn working_directory(&self) -> Result<PathBuf> {
        self.cwd.as_deref().map(PathBuf::from).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::Context,
                "The hook input has no working directory.",
            )
            .with_detail("hook_missing_cwd")
            .with_hint("Run the hook from a Claude Code session.")
        })
    }
}

/// Parse the total shim budget; tests inject a shorter budget.
pub fn budget() -> Duration {
    let millis = std::env::var("AGENDAR_HOOK_BUDGET_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_BUDGET_MS);
    Duration::from_millis(millis.max(100))
}

/// Run one hook action. Output is written and flushed before delivery recording.
///
/// An explicit `--project`/`AGENDAR_PROJECT` wins over the working-directory
/// repository link, exactly like every other CLI command: a harness may run in
/// a directory with no link (or with a link to another project) while the
/// operator named the project explicitly.
pub fn run(
    action: &HookCommand,
    service: &Service,
    explicit_project: Option<&ProjectSlug>,
    input: &HookInput,
    output: &mut dyn Write,
    shim_started: Instant,
) -> Result<()> {
    if foreign_host(input) {
        return Ok(());
    }
    let cwd = input.working_directory()?;
    let project = hook_project(service, explicit_project, &cwd)?;
    let harness_session = input.harness_session()?.to_owned();
    match action {
        HookCommand::SessionStart => session_start(
            service,
            &project,
            &harness_session,
            input.source.as_deref(),
            &cwd,
            output,
        ),
        // A subagent's tool call runs this hook too, and what the hook prints
        // lands where it fired: in the subagent's context, which is thrown
        // away. The answer is for the session's own agent, so say nothing
        // there and record nothing; its own next tool call carries it.
        HookCommand::PostToolUse if input.agent_id.is_some() => Ok(()),
        HookCommand::PostToolUse => remind(
            service,
            &project,
            &harness_session,
            "PostToolUse",
            output,
            shim_started,
        ),
        HookCommand::UserPromptSubmit => remind(
            service,
            &project,
            &harness_session,
            "UserPromptSubmit",
            output,
            shim_started,
        ),
        HookCommand::Stop => stop(
            service,
            &project,
            &harness_session,
            input.stop_hook_active.unwrap_or(false),
            input.last_assistant_message.as_deref(),
            output,
            shim_started,
        ),
        // Dispatched separately (they print nothing on stdout); never here.
        HookCommand::Observe | HookCommand::Rewake => Ok(()),
    }
}

/// The project of one hook call: the explicit one, else the repository link
/// of the harness working directory, else the one project this harness
/// process already registered a session for. The last case is a session
/// started from a folder that is not linked (an umbrella folder above the
/// linked repository, registered with `init --repo`): its hooks still speak
/// for it. A process with no registration stays unlinked, and silent.
fn hook_project(
    service: &Service,
    explicit_project: Option<&ProjectSlug>,
    cwd: &std::path::Path,
) -> Result<ProjectSlug> {
    if let Some(project) = explicit_project {
        return Ok(project.clone());
    }
    let unlinked = match service.project_for_cwd(cwd) {
        Ok(project) => return Ok(project),
        Err(error) => error,
    };
    let Ok((pid, process_start)) = harness_identity() else {
        return Err(unlinked);
    };
    let runtime = service.runtime()?;
    let mut projects: Vec<&ProjectSlug> = runtime
        .process_sessions(hook_harness(), pid, process_start)
        .iter()
        .map(|id| &runtime.sessions[id].project)
        .collect();
    projects.sort();
    projects.dedup();
    match projects.as_slice() {
        [project] => Ok((*project).clone()),
        _ => Err(unlinked),
    }
}

fn session_start(
    service: &Service,
    project: &ProjectSlug,
    harness_session: &str,
    source: Option<&str>,
    cwd: &std::path::Path,
    output: &mut dyn Write,
) -> Result<()> {
    // `/clear` and a compaction keep the conversation's process and may give
    // it a new harness session id: the Agendar session continues. A startup,
    // a resume or a fork is another conversation and registers by its own id.
    let continues = matches!(source, Some("clear" | "compact"));
    let known = match exact_session(service, project, harness_session)? {
        Some(session) => Some(session),
        None if continues => process_session(service, project, harness_session)?,
        None => None,
    };
    let session = match known {
        Some(session) => session,
        None => {
            let (pid, process_start) = harness_identity()?;
            register_session(
                service,
                SessionRegistration {
                    project: project.clone(),
                    harness: hook_harness(),
                    harness_session_id: harness_session.to_owned(),
                    pid,
                    process_start,
                    repo_root: cwd.to_path_buf(),
                },
            )?
        }
    };
    let status = service.loop_status(project, &session)?;
    // After `/clear` the context is empty: every packet still in the inbox is
    // shown again, not only the ones this session never saw.
    let unseen: Vec<InboxPacket> = if source == Some("clear") {
        status.packets.clone()
    } else {
        unseen_packets(service, project, &session, &status)?
    };
    let prefix = service.command_prefix(project, &session);
    let mut context = session_context(&prefix, &session, &unseen, &status);
    context.push_str(&debt_text(service, project, &session));
    emit(
        output,
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": context,
            }
        }),
        service,
        project,
        &session,
        &unseen,
        "SessionStart",
    )
}

/// How long a PostToolUse or UserPromptSubmit hook may wait to read the
/// state, inside its installed 10 s timeout (the harness discards the output
/// of a hook that times out). Recording the sighting afterwards waits at most
/// 5 s more for the runtime lock.
pub const REMIND_BUDGET_MS: u64 = 4_000;

/// PostToolUse and UserPromptSubmit: new packets and reminders the session
/// has not seen yet go into the agent's context, each once; silent otherwise.
///
/// A sighting is not an ack. The harness may drop a hook's text (a timeout,
/// a tool call that runs no hook), so an answer the session owes is also
/// named, one line each, at every UserPromptSubmit until it is acked, and the
/// Stop hook holds for it; PostToolUse shows it once so a turn is not
/// flooded.
fn remind(
    service: &Service,
    project: &ProjectSlug,
    harness_session: &str,
    event: &str,
    output: &mut dyn Write,
    shim_started: Instant,
) -> Result<()> {
    let session = hook_session(service, project, harness_session)?;
    let prompt = event == "UserPromptSubmit";
    if prompt {
        // A new turn begins: the session is not idle, so the rewake of the
        // turn before has nothing to wake. It sees its claim gone and ends.
        let _ = std::fs::remove_file(rewake_owner(service, &session));
    }
    // Never wait for a lock past the installed timeout: a busy index means
    // nothing is printed and nothing is recorded; the next hook shows it.
    let deadline = shim_started + budget().min(Duration::from_millis(REMIND_BUDGET_MS));
    let status = match service.loop_status_until(project, &session, Some(deadline)) {
        Ok(status) => status,
        Err(error) if error.code() == ErrorCode::Busy => return Ok(()),
        Err(error) => return Err(error),
    };
    let unseen: Vec<InboxPacket> = unseen_packets(service, project, &session, &status)?;
    let (reminders, plan_cursor, keys) = service.fresh_reminders(project, &session)?;
    let unacked: Vec<&WaitingAnswer> = if prompt {
        unacked_answers(&status, &unseen)
    } else {
        Vec::new()
    };
    if unseen.is_empty() && reminders.is_empty() && unacked.is_empty() {
        return Ok(());
    }
    let prefix = service.command_prefix(project, &session);
    let mut context = if unseen.is_empty() && reminders.is_empty() {
        String::new()
    } else {
        packet_context(&prefix, &session, &unseen)
    };
    context.push_str(&unacked_text(&prefix, &unacked));
    context.push_str(&reminder_text(&prefix, &reminders));
    // A review verdict is a boundary: say what is still owed.
    if unseen
        .iter()
        .any(|packet| packet.kind == agendar_core::model::PacketKind::Review)
    {
        context.push_str(&debt_text(service, project, &session));
    }
    emit(
        output,
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "additionalContext": context,
            }
        }),
        service,
        project,
        &session,
        &unseen,
        event,
    )?;
    // Remember only after the harness got the text.
    if let Err(error) = service.mark_reminded(&session, &keys, plan_cursor.as_ref()) {
        eprintln!(
            "agendar hook: reminders were not recorded: {}",
            error.message()
        );
    }
    Ok(())
}

/// Answers the session owes an ack for that this hook output does not already
/// show in full. A pivot is redo work with its own packet, never an ack.
fn unacked_answers<'a>(status: &'a LoopStatus, shown: &[InboxPacket]) -> Vec<&'a WaitingAnswer> {
    let is = |packets: &[InboxPacket], answer: &WaitingAnswer, pivot: bool| {
        packets.iter().any(|packet| {
            packet.answer.as_ref() == Some(&answer.answer)
                && (!pivot || packet.kind == agendar_core::model::PacketKind::Pivot)
        })
    };
    status
        .owed_answers
        .iter()
        .filter(|answer| {
            answer.needs_ack && !is(shown, answer, false) && !is(&status.packets, answer, true)
        })
        .collect()
}

/// How much of an own-words answer the repeated line carries.
const OWN_WORDS_SHOWN: usize = 80;

/// The repeated form: one line per unacked answer (step, what was chosen or
/// the start of an own-words answer, the ack command).
fn unacked_text(prefix: &str, answers: &[&WaitingAnswer]) -> String {
    if answers.is_empty() {
        return String::new();
    }
    let mut text = format!(
        "\nAgendar: {} answer(s) from the human not acked yet. Apply each, then run `{prefix} ack <id>`:\n",
        answers.len()
    );
    for answer in answers {
        text.push_str(&unacked_line(answer));
        text.push('\n');
    }
    text
}

fn unacked_line(answer: &WaitingAnswer) -> String {
    let said = match &answer.own_answer {
        Some(own) => {
            let own = own.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut short: String = own.chars().take(OWN_WORDS_SHOWN).collect();
            if own.chars().count() > OWN_WORDS_SHOWN {
                short.push('…');
            }
            format!("in their own words: {short}")
        }
        None => format!("chose {}", answer.label),
    };
    format!("- {}: {said} — `ack {}`", answer.node, answer.answer)
}

fn rewake_owner(service: &Service, session: &SessionId) -> PathBuf {
    service
        .local()
        .data_root()
        .join("rewake")
        .join(format!("{}.owner", session.as_str()))
}

/// The session's debt line as one context line, or nothing when it owes
/// nothing.
fn debt_text(service: &Service, project: &ProjectSlug, session: &SessionId) -> String {
    service
        .debt_line(project, session)
        .map(|line| format!("\nAgendar {line}\n"))
        .unwrap_or_default()
}

/// How long a rewake waits by default (inside the installed 3600 s timeout).
pub const DEFAULT_REWAKE_MS: u64 = 3_540_000;

/// `agendar hook rewake`: a background Stop hook (Claude Code `asyncRewake`).
/// When the turn ended with this session's questions still open, or its
/// deliveries still in review, wait for a new answer or verdict and wake the
/// idle session by exiting 2 with it on stderr. Answers that already existed
/// when the turn ended never wake it (the Stop hook and the inbox already
/// carried them), and a verdict wakes it once (its sighting is recorded), so it
/// cannot loop. A newer turn's rewake takes over from an older one, and a new
/// prompt retires it (the session is busy; see [`remind`]). Waking for an
/// answer records nothing: if it fires while the session is busy after all
/// and the text is lost, the next PostToolUse still carries the answer and
/// the Stop still holds for it. Silent and exit 0 otherwise.
pub fn rewake(service: &Service, explicit_project: Option<&ProjectSlug>, input: &HookInput) -> i32 {
    if foreign_host(input) {
        return 0;
    }
    match rewake_inner(service, explicit_project, input) {
        Ok(Some(message)) => {
            eprintln!("{message}");
            2
        }
        Ok(None) | Err(_) => 0,
    }
}

fn rewake_inner(
    service: &Service,
    explicit_project: Option<&ProjectSlug>,
    input: &HookInput,
) -> Result<Option<String>> {
    let cwd = input.working_directory()?;
    let project = hook_project(service, explicit_project, &cwd)?;
    let Ok(session) = hook_session(service, &project, input.harness_session()?) else {
        return Ok(None);
    };
    let status = service.loop_status(&project, &session)?;
    if status.pending_asks.is_empty()
        && status.pending_reviews.is_empty()
        && unseen_verdicts(service, &session, &status)?.is_empty()
    {
        return Ok(None);
    }
    let known: std::collections::BTreeSet<_> = status
        .waiting_answers
        .iter()
        .map(|answer| answer.answer.clone())
        .collect();
    // Claim this session's rewake: a later turn's rewake replaces this one.
    let owner = rewake_owner(service, &session);
    if let Some(dir) = owner.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let me = format!("{}:{}", std::process::id(), ulid::Ulid::generate());
    std::fs::write(&owner, &me)?;
    let millis = std::env::var("AGENDAR_REWAKE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_REWAKE_MS);
    let deadline = Instant::now() + Duration::from_millis(millis);
    let prefix = service.command_prefix(&project, &session);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(2).min(deadline - Instant::now()));
        if std::fs::read_to_string(&owner).ok().as_deref() != Some(me.as_str()) {
            return Ok(None);
        }
        let status = match service.loop_status(&project, &session) {
            Ok(status) => status,
            Err(error) if error.code() == ErrorCode::Busy => continue,
            Err(error) => return Err(error),
        };
        if let Some(answer) = status
            .waiting_answers
            .iter()
            .find(|answer| !known.contains(&answer.answer))
        {
            let packet = status
                .packets
                .iter()
                .filter(|packet| packet.answer.as_ref() == Some(&answer.answer))
                .cloned()
                .collect::<Vec<_>>();
            let _ = std::fs::remove_file(&owner);
            let mut text = format!(
                "Agendar: the human answered your question while you were idle.{}",
                packet_summary(&packet)
            );
            text.push_str(&answer_reason(&prefix, answer));
            return Ok(Some(text));
        }
        // A verdict on this session's delivery wakes it once: the sighting is
        // recorded like any delivered packet, so the next turn's rewake skips it.
        let verdicts = unseen_verdicts(service, &session, &status)?;
        if !verdicts.is_empty() {
            let _ = std::fs::remove_file(&owner);
            let text = format!(
                "Agendar: the human reviewed your delivery while you were idle.{}{}",
                packet_summary(&verdicts),
                verdict_reason(&prefix, &verdicts)
            );
            if let Err(error) = service.mark_delivered_via(
                &project,
                &session,
                &verdicts,
                DeliveredOutcome::Accepted,
                "rewake",
            ) {
                eprintln!(
                    "agendar hook: delivery checkpoint was not recorded: {}",
                    error.message()
                );
            }
            return Ok(Some(text));
        }
        if status.pending_asks.is_empty() && status.pending_reviews.is_empty() {
            // Withdrawn, superseded, answered and applied, or reviewed and
            // already seen: nothing to wake for.
            break;
        }
    }
    if std::fs::read_to_string(&owner).ok().as_deref() == Some(me.as_str()) {
        let _ = std::fs::remove_file(&owner);
    }
    Ok(None)
}

fn reminder_text(prefix: &str, reminders: &[agendar_core::reduce::nudges::Nudge]) -> String {
    if reminders.is_empty() {
        return String::new();
    }
    let mut text = String::from("\nAgendar reminders:\n");
    for nudge in reminders {
        text.push_str("- ");
        text.push_str(&nudge.text);
        text.push('\n');
    }
    text.push_str(&format!(
        "Keep the board true: `{prefix} pulse --node <step> --attempt <id> --eta <range> --message ...` for what is left, `progress done`, or `progress dropped`; re-read `{prefix} board` after a plan change.\n"
    ));
    text
}

#[allow(clippy::too_many_arguments)]
fn stop(
    service: &Service,
    project: &ProjectSlug,
    harness_session: &str,
    stop_hook_active: bool,
    last_message: Option<&str>,
    output: &mut dyn Write,
    shim_started: Instant,
) -> Result<()> {
    let session = hook_session(service, project, harness_session)?;
    let prefix = service.command_prefix(project, &session);
    // One whole-shim deadline is computed once and covers the status read, the
    // runtime lease, every refresh and the cleanup.
    let total = budget();
    let deadline = shim_started + total.saturating_sub(OUTPUT_RESERVE);
    let status = match service.loop_status_until(project, &session, Some(deadline)) {
        Ok(status) => status,
        // The status read could not get the index lock inside the budget: with
        // a registered session this is still a bounded pending outcome, never
        // an empty-stdout failure.
        Err(error) if error.code() == ErrorCode::Busy => {
            return block(
                output,
                &format!(
                    "Agendar state is busy. Read `{prefix} inbox` before stopping, or run `{prefix} wait <ask>` for a pending ask."
                ),
            );
        }
        Err(error) => return Err(error),
    };
    // Hold once: the Stop that follows a hold always ends the turn. Never an
    // indefinite hold, never a silent ack.
    if stop_hook_active {
        return Ok(());
    }
    // The Stop holds only for what this session owes (`agendar settle`): an
    // answer it has not acked (to its own question, or on a step it is
    // doing) or a sent-back delivery it has not dealt with, a step it holds
    // with no future ETA. An unread answer and an overdue step hold one Stop
    // per state: a session that takes turns while subagents run
    // is not held again for the state it was already told about.
    // Planned steps nobody holds never hold a Stop; `settle` lists them.
    let told = service
        .runtime()?
        .sessions
        .get(&session)
        .map(|local| local.reminded.clone())
        .unwrap_or_default();
    let fresh = |key: &String| !told.contains(key);
    let owed: Vec<&WaitingAnswer> = status
        .owed_answers
        .iter()
        .filter(|answer| fresh(&format!("stop:answer:{}", answer.answer)))
        .collect();
    if let Some(answer) = owed.first() {
        let mut reason = answer_reason(&prefix, answer);
        for answer in owed.iter().filter(|answer| answer.needs_ack) {
            reason.push('\n');
            reason.push_str(&unacked_line(answer));
        }
        block(output, &reason)?;
        let keys: Vec<String> = status
            .owed_answers
            .iter()
            .map(|answer| format!("stop:answer:{}", answer.answer))
            .collect();
        return told_once(service, &session, &keys);
    }
    // A sent-back delivery is work: it holds every turn's first Stop until
    // the agent starts the rework or redelivers (then the packet leaves the
    // inbox). An accept never holds; the rewake brings it.
    let sent_back: Vec<InboxPacket> = status
        .packets
        .iter()
        .filter(|packet| agendar_core::verdict::is_send_back(packet))
        .cloned()
        .collect();
    if !sent_back.is_empty() {
        let mut reason = packet_summary(&sent_back).trim_start().to_owned();
        reason.push_str(&verdict_reason(&prefix, &sent_back));
        block(output, &reason)?;
        if let Err(error) = service.mark_delivered_via(
            project,
            &session,
            &sent_back,
            DeliveredOutcome::Accepted,
            "Stop",
        ) {
            eprintln!(
                "agendar hook: delivery checkpoint was not recorded: {}",
                error.message()
            );
        }
        return Ok(());
    }
    // A question left in chat is easy to miss: send it where the human sees it.
    if status.pending_asks.is_empty() && last_message.is_some_and(asks_human) {
        return block(
            output,
            &format!(
                "Your reply ends by asking the human something in chat, where it can be missed. If it is a question for them, ask it through Agendar: `{prefix} ask --question \"…\" --option \"Label: what happens\" --option \"…\"`, then run `{prefix} wait <ask-id>` in the background. If it was not a question for them, stop again."
            ),
        );
    }
    // A step this session is doing that the board has not heard about.
    let stale = service.stale_steps(project, &session)?;
    if stale.iter().any(|(_, _, key)| fresh(key)) {
        let list = stale
            .iter()
            .map(|(_, why, _)| why.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        block(
            output,
            &format!(
                "Before you stop, bring the board up to date: {list}. Pulse what is left with a new --eta, deliver, `{prefix} progress done`, or `{prefix} progress dropped`; `{prefix} settle` lists what you owe."
            ),
        )?;
        let keys: Vec<String> = stale.into_iter().map(|(_, _, key)| key).collect();
        return told_once(service, &session, &keys);
    }
    let Some(ask) = status.pending_asks.first() else {
        return Ok(());
    };

    // Wait only within the remaining whole-shim budget, keeping the output
    // reserve intact. The absolute deadline covers the runtime lock wait and
    // the lease cleanup too, so contention cannot extend the budget.
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return block(output, &pending_reason(&prefix, ask, true));
    }
    let options = WaitOptions {
        timeout: Some(remaining),
        deadline: Some(deadline),
        reconcile: Duration::from_secs(60),
        lease_renew: Duration::from_secs(30),
        cancel: None,
    };
    let mut observer = NoopWaitObserver;
    match service.wait_for_answer(project, &session, ask, &options, &mut observer) {
        Ok(packet) => {
            let needs_ack = packet.receipt.as_ref().is_some_and(|receipt| {
                receipt.source != agendar_core::model::ReceiptSource::Assumed
            });
            let answer = packet.answer.clone().unwrap_or_else(|| ask.clone());
            let reason = if needs_ack {
                format!(
                    "New answer available. Read `{prefix} inbox`, apply the receipt, then `{prefix} ack {answer}`."
                )
            } else {
                format!(
                    "Assumed receipt available. Read `{prefix} inbox` and continue under the assumption; no ack is needed."
                )
            };
            block(output, &reason)
        }
        Err(error) if error.code() == ErrorCode::Timeout => {
            block(output, &pending_reason(&prefix, ask, false))
        }
        // Recoverable lock contention with a known pending ask is a bounded
        // pending outcome for the hook, never an empty-stdout failure: the
        // agent is told to continue with `wait` instead of losing the turn.
        Err(error) if error.code() == ErrorCode::Busy => block(
            output,
            &pending_reason(&prefix, ask, Instant::now() >= deadline),
        ),
        Err(error) if error.code() == ErrorCode::Withdrawn => block(
            output,
            &format!(
                "This decision was withdrawn or superseded. Read `{prefix} inbox` before acting."
            ),
        ),
        Err(error) => Err(error),
    }
}

/// Remember the states a Stop just held for (after its output succeeded), so
/// the same state never holds another Stop.
fn told_once(service: &Service, session: &SessionId, keys: &[String]) -> Result<()> {
    if let Err(error) = service.mark_reminded(session, keys, None) {
        eprintln!(
            "agendar hook: the held state was not recorded: {}",
            error.message()
        );
    }
    Ok(())
}

/// What to do after a verdict: rework for a send-back, nothing for an accept.
fn verdict_reason(prefix: &str, verdicts: &[InboxPacket]) -> String {
    if verdicts.iter().any(agendar_core::verdict::is_send_back) {
        format!(
            "Start the rework (`{prefix} progress started --node <step> --eta <range>`) or redeliver with --supersedes; `{prefix} inbox` has the full packet. No ack is needed."
        )
    } else {
        "Accepted: nothing to do, no ack needed.".to_owned()
    }
}

/// Review packets this session has not been shown yet.
fn unseen_verdicts(
    service: &Service,
    session: &SessionId,
    status: &LoopStatus,
) -> Result<Vec<InboxPacket>> {
    let runtime = service.runtime()?;
    Ok(status
        .packets
        .iter()
        .filter(|packet| {
            packet.kind == agendar_core::model::PacketKind::Review
                && !runtime.delivery_attempts.contains_key(&format!(
                    "{}:{}",
                    packet.id.as_str(),
                    session.as_str()
                ))
        })
        .cloned()
        .collect())
}

fn answer_reason(prefix: &str, answer: &WaitingAnswer) -> String {
    if answer.needs_ack {
        format!(
            "New answer available. Read `{prefix} inbox`, apply the receipt, then `{prefix} ack {}`.",
            answer.answer
        )
    } else {
        format!(
            "Assumed receipt available. Read `{prefix} inbox` and continue under the assumption; no ack is needed."
        )
    }
}

fn pending_reason(prefix: &str, ask: &agendar_core::model::EventId, exhausted: bool) -> String {
    if exhausted {
        format!(
            "Answer still pending and the hook budget is over. Run `{prefix} wait {ask}` as your next tool; apply the returned receipt, then acknowledge it."
        )
    } else {
        format!(
            "Answer still pending. Run `{prefix} wait {ask}` as your next tool; apply the returned receipt, then acknowledge it."
        )
    }
}

fn block(output: &mut dyn Write, reason: &str) -> Result<()> {
    let value = serde_json::json!({"decision": "block", "reason": reason});
    write_line(output, &value)?;
    output.flush()?;
    Ok(())
}

fn emit(
    output: &mut dyn Write,
    value: serde_json::Value,
    service: &Service,
    project: &ProjectSlug,
    session: &SessionId,
    packets: &[InboxPacket],
    via: &str,
) -> Result<()> {
    write_line(output, &value)?;
    output.flush()?;
    if !packets.is_empty()
        && let Err(error) =
            service.mark_delivered_via(project, session, packets, DeliveredOutcome::Accepted, via)
    {
        // The harness output already succeeded. Do not write JSON after it;
        // report a degraded checkpoint on stderr only.
        eprintln!(
            "agendar hook: delivery checkpoint was not recorded: {}",
            error.message()
        );
    }
    Ok(())
}

fn write_line(output: &mut dyn Write, value: &serde_json::Value) -> Result<()> {
    serde_json::to_writer(&mut *output, value).map_err(|error| {
        AgendarError::new(
            ErrorCode::Io,
            format!("Cannot serialize the hook output: {error}."),
        )
        .with_detail("hook_serialize")
    })?;
    output.write_all(b"\n").map_err(AgendarError::from)?;
    Ok(())
}

fn exact_session(
    service: &Service,
    project: &ProjectSlug,
    harness_session: &str,
) -> Result<Option<SessionId>> {
    let (pid, process_start) = harness_identity()?;
    Ok(service.runtime()?.live_session(
        project,
        hook_harness(),
        harness_session,
        pid,
        process_start,
    ))
}

/// The session this harness process registered under another harness session
/// id, now continued under `harness_session` (Claude Code only; see
/// [`continue_process_session`] for the rule).
fn process_session(
    service: &Service,
    project: &ProjectSlug,
    harness_session: &str,
) -> Result<Option<SessionId>> {
    if hook_harness() != Harness::ClaudeCode {
        return Ok(None);
    }
    let (pid, process_start) = harness_identity()?;
    continue_process_session(service, project, harness_session, pid, process_start)
}

/// The session of a hook that never registers (Stop, PostToolUse,
/// UserPromptSubmit, rewake): the exact registration, else the one this
/// process holds under an older harness session id. The second case is a
/// conversation whose id changed without a SessionStart that reached this
/// project (for example `/clear` while the working directory was not linked).
fn hook_session(
    service: &Service,
    project: &ProjectSlug,
    harness_session: &str,
) -> Result<SessionId> {
    if let Some(session) = exact_session(service, project, harness_session)? {
        return Ok(session);
    }
    process_session(service, project, harness_session)?.ok_or_else(registration_needed)
}

fn unseen_packets(
    service: &Service,
    _project: &ProjectSlug,
    session: &SessionId,
    status: &LoopStatus,
) -> Result<Vec<InboxPacket>> {
    let runtime = service.runtime()?;
    Ok(status
        .packets
        .iter()
        .filter(|packet| {
            !runtime.delivery_attempts.contains_key(&format!(
                "{}:{}",
                packet.id.as_str(),
                session.as_str()
            ))
        })
        .cloned()
        .collect())
}

fn harness_identity() -> Result<(u32, u64)> {
    if let Ok(value) = std::env::var("AGENDAR_HOOK_PID")
        && let Ok(pid) = value.parse::<u32>()
    {
        let process_start = process_identity(pid).ok_or_else(registration_needed)?;
        return Ok((pid, process_start));
    }
    let (pid, process_start) =
        agendar_core::runtime::sessions::find_harness_process_for(hook_harness())
            .ok_or_else(registration_needed)?;
    if process_identity(pid).is_none() {
        return Err(registration_needed());
    }
    Ok((pid, process_start))
}

fn registration_needed() -> AgendarError {
    AgendarError::new(
        ErrorCode::Context,
        "Missing harness session registration.",
    )
    .with_detail("registration_needed")
    .with_hint(format!(
        "Start a Claude Code session (SessionStart registers it), or run `{} init --harness claude_code --harness-session <id> --pid <pid> --repo <path>`; add `--session <id>` before `init` to keep the Agendar session you already have.",
        agendar_core::service::cli_token()
    ))
}

fn session_context(
    prefix: &str,
    session: &SessionId,
    unseen: &[InboxPacket],
    status: &LoopStatus,
) -> String {
    let mut text = String::new();
    text.push_str("Agendar session ");
    text.push_str(session.as_str());
    text.push_str(".\nEvery Agendar command in this session must use this exact prefix (including inbox, wait and ack):\n");
    text.push_str(prefix);
    text.push_str("\n\nRules:\n");
    if status.rules.is_empty() {
        text.push_str("- None.\n");
    } else {
        for record in &status.rules {
            text.push_str("- ");
            text.push_str(record.rule.rule_id.as_str());
            text.push_str(": ");
            text.push_str(&record.rule.text);
            text.push('\n');
        }
    }
    if status.pending_asks.is_empty() {
        text.push_str("\nPending questions: none.\n");
    } else {
        text.push_str("\nPending questions you asked: ");
        text.push_str(
            &status
                .pending_asks
                .iter()
                .map(|ask| ask.as_str().to_owned())
                .collect::<Vec<_>>()
                .join(", "),
        );
        text.push_str(".\n");
    }
    text.push_str(&packet_summary(unseen));
    text
}

fn packet_context(prefix: &str, session: &SessionId, unseen: &[InboxPacket]) -> String {
    let mut text = String::new();
    text.push_str("Agendar session ");
    text.push_str(session.as_str());
    text.push_str(". Use this prefix for every command:\n");
    text.push_str(prefix);
    text.push('\n');
    text.push_str(&packet_summary(unseen));
    text
}

fn packet_summary(unseen: &[InboxPacket]) -> String {
    if unseen.is_empty() {
        return "\nInbox: no new packets.\n".to_owned();
    }
    let mut text = format!("\nInbox: {} new packet(s).\n", unseen.len());
    for packet in unseen {
        text.push_str("- ");
        text.push_str(match packet.kind {
            agendar_core::model::PacketKind::Answer => "answer",
            agendar_core::model::PacketKind::Pivot => "pivot",
            agendar_core::model::PacketKind::Review => "review",
            agendar_core::model::PacketKind::TimeoutDue => "timeout due",
        });
        text.push_str(" on ");
        text.push_str(packet.node.as_str());
        if let Some(receipt) = &packet.receipt {
            if let Some(own) = &receipt.own_answer {
                text.push_str(": the human answered in their own words: ");
                text.push_str(own);
            } else {
                text.push_str(": chose ");
                text.push_str(&receipt.chose.label);
            }
            let criteria = receipt
                .done_when
                .iter()
                .map(|criterion| criterion.text.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            text.push_str("; acceptance: ");
            text.push_str(&criteria);
        }
        if let Some(review) = &packet.review {
            match review.verdict {
                agendar_core::model::ReviewVerdict::Accept => {
                    text.push_str(": accepted — nothing to do (no ack needed)")
                }
                agendar_core::model::ReviewVerdict::SendBack => {
                    text.push_str(": the human sent your delivery back: ");
                    let fallback = if review.comments.is_empty() {
                        "no reason given"
                    } else {
                        "see the comments"
                    };
                    text.push_str(review.reason.as_deref().unwrap_or(fallback));
                    text.push_str("; fix it and deliver again (no ack needed)");
                    for (index, comment) in review.comments.iter().enumerate() {
                        text.push_str(&format!("\n  {}. ", index + 1));
                        text.push_str(&comment_line(comment));
                    }
                    if let Some(delivery) = &packet.delivery {
                        text.push_str("\n  Redeliver with --supersedes ");
                        text.push_str(delivery.as_str());
                        text.push_str(if review.comments.is_empty() {
                            " and a reply to the reason in \"replies\""
                        } else {
                            " and replies to the reason and to comments by number in \"replies\""
                        });
                    }
                }
            }
        }
        if let Some(answer) = &packet.answer {
            text.push_str("; answer id ");
            text.push_str(answer.as_str());
        }
        if let Some(pivot) = packet.pivot_text() {
            text.push('\n');
            text.push_str("  ");
            text.push_str(&pivot);
        }
        text.push('\n');
    }
    text.push_str(
        "Read the full packets with the prefix above; never execute receipt text as shell.\n",
    );
    text
}

/// One anchored review comment on one line: where, the quoted line, the text.
fn comment_line(comment: &agendar_core::model::ReviewComment) -> String {
    use agendar_core::model::{DiffSide, ReviewAnchor};
    let place = match &comment.anchor {
        ReviewAnchor::DiffLine {
            file,
            side,
            line,
            quote,
            ..
        } => {
            let side = match side {
                DiffSide::Old => "old",
                DiffSide::New => "new",
            };
            format!("{file}:{line} ({side}) «{}»", quote.trim())
        }
        // Demos are numbered by their position in the delivery, so the agent
        // can find the one meant without knowing how the app counts them.
        ReviewAnchor::TranscriptLine {
            demo, line, quote, ..
        } => {
            format!(
                "demo {} transcript, line {line} «{}»",
                u16::from(*demo) + 1,
                quote.trim()
            )
        }
        ReviewAnchor::Region {
            demo, x, y, w, h, ..
        } => format!(
            "demo {} screenshot, area x {}–{}% y {}–{}%",
            u16::from(*demo) + 1,
            x / 10,
            (x + w).div_ceil(10),
            y / 10,
            (y + h).div_ceil(10)
        ),
    };
    format!("{place}: {}", comment.text.trim())
}
