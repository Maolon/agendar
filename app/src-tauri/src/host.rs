//! Host state and command implementations.
//!
//! Everything here runs without a Tauri window so the same logic is exercised
//! by `cargo test -p agendar-app`: the IPC wrappers in `commands` only add the
//! native caller check and the wire envelope.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use agendar_core::error::{AgendarError, ErrorCode};
use agendar_core::index::Index;
use agendar_core::index::query::recent_project_events;
use agendar_core::model::{
    BlobExt, BlobRef, CheckResult, Confidence, CriterionId, DecisionLifecycle, DecisionState,
    Event, EventId, EventPayload, EvidenceObserved, Harness, NeedsKind, ObservationMethod,
    OptionId, PlanFeature, PolicySet, ProjectDeclared, ProjectSlug, ProjectState, ReceiptSource,
    RescueAction, Reversibility, ReviewVerdict, RuleSet, Sha256, State, UtcTime,
};
use agendar_core::reduce::activity;
use agendar_core::reduce::stats as project_stats;
use agendar_core::runtime::checks::{self as check_runtime, CheckProfile};
use agendar_core::runtime::install::{
    HookSet, home_dir, install_claude_hook_set, install_claude_hooks, install_path_link,
    plan_claude_hook_set, plan_claude_hooks, plan_path_link,
};
use agendar_core::runtime::{Config, LocalPaths, VaultWatch};
use agendar_core::service::{Fresh, Service};
use agendar_core::vault::blobs::{put_blob, read_blob};
use agendar_core::vault::{Clock as _, Vault};

use crate::error::{ApiResult, AppError};
use crate::guard::{
    self, Guard, PreparedToken, TokenStore, effective_risk, new_notice_id, new_request_id,
    new_token, receipt_preview,
};
use crate::notifications::{self, Availability, NoticeSink, NoticeSpec};
use crate::preferences::Preferences;
use crate::projection;
use crate::ui::Ui;

/// Wall clock injection so a test can prove the 60 s token TTL.
pub trait WallClock: Send + Sync {
    fn now(&self) -> UtcTime;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now(&self) -> UtcTime {
        agendar_core::vault::SystemClock.now()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeAction {
    Open,
    /// The answer button at this index of the notice's choices.
    Choose(usize),
}

/// Where an action came from. A late/cold callback may only navigate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Live,
    Late,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeOutcome {
    Focused { item_id: String, sequence: u64 },
    Answered { duplicate: bool },
    Routed { item_id: String, status: String },
    Unknown,
}

#[derive(Debug, Clone)]
struct JournalEntry {
    command: String,
    digest: Sha256,
    data: Value,
}

#[derive(Debug, Clone)]
struct RequestEntry {
    command: &'static str,
    digest: Sha256,
    data: Value,
}

/// One answer button of a shown notice.
#[derive(Debug, Clone)]
struct NoticeChoice {
    token: String,
    request_id: String,
    label: String,
}

#[derive(Debug, Clone)]
struct NoticeBinding {
    item_id: String,
    project: ProjectSlug,
    /// The answer buttons, in the order the notice shows them.
    choices: Vec<NoticeChoice>,
    answered: bool,
    /// The visible notice was taken down; the binding stays so a late click
    /// still routes (and a repeated approve still reports a duplicate).
    withdrawn: bool,
}

/// One expiring opaque media capability returned by `read_report`.
#[derive(Debug, Clone)]
struct MediaToken {
    project: ProjectSlug,
    blob: BlobRef,
    mime: String,
    expires_at: UtcTime,
}

#[derive(Debug, Default)]
struct Navigation {
    epoch: String,
    next_sequence: u64,
    focus: Option<Value>,
}

struct Inner {
    config: Config,
    preferences: Preferences,
    service: Option<std::sync::Arc<Service>>,
    open_error: Option<AgendarError>,
    tokens: TokenStore,
    journal: BTreeMap<String, JournalEntry>,
    notices: BTreeMap<String, NoticeBinding>,
    notice_dedupe: BTreeSet<String>,
    /// Per-project progress seen at the last tick; `None` until the first tick
    /// seeds it silently, so a restart never replays old milestones.
    progress_marks: Option<BTreeMap<ProjectSlug, notifications::ProgressMark>>,
    navigation: Navigation,
    media_tokens: BTreeMap<String, MediaToken>,
    requests: BTreeMap<String, RequestEntry>,
    epoch: String,
    /// Event IDs this host has already published, so a change batch reports
    /// each real pulse exactly once.
    seen_events: BTreeSet<EventId>,
    /// The previously published projection, used only to derive the released
    /// set of an `agendar://unblock` transition.
    previous_state: Option<State>,
    /// Unblock notifications already emitted, keyed `<phase>:<event>`.
    unblocked: BTreeSet<String>,
    /// The released set of each answer, kept so the `acked` phase reports the
    /// same features the `answered` phase released (the release has already
    /// happened by then, so it cannot be recomputed from a before/after diff).
    releases: BTreeMap<EventId, Vec<String>>,
    /// Whether the initial history has been seeded. Only the first read (or the
    /// first tick) consumes publication state; a later read must never swallow
    /// pulses that still have to animate.
    hydrated: bool,
}

/// The shared app host.
pub struct Host {
    data_root: PathBuf,
    clock: std::sync::Arc<dyn WallClock>,
    ui: std::sync::Arc<dyn Ui>,
    sink: std::sync::Arc<dyn NoticeSink>,
    inner: Mutex<Inner>,
    shutdown: AtomicBool,
    /// Token counts from the agents' local logs, refreshed once a minute.
    usage: Mutex<agendar_core::runtime::usage::UsageScanner>,
}

impl Host {
    pub fn new(
        data_root: impl AsRef<Path>,
        clock: std::sync::Arc<dyn WallClock>,
        ui: std::sync::Arc<dyn Ui>,
        sink: std::sync::Arc<dyn NoticeSink>,
    ) -> ApiResult<Self> {
        let local = LocalPaths::new(data_root).map_err(AppError::from)?;
        let data_root = local.data_root().to_path_buf();
        let config = Config::load_or_default(&data_root).map_err(AppError::from)?;
        let preferences = Preferences::load(&data_root).unwrap_or_default();
        crate::preferences::set_active_lang(crate::preferences::Lang::resolve(
            &preferences.language,
        ));
        let mut service = None;
        let mut open_error = None;
        if let Some(vault) = &config.vault {
            match Service::open(Path::new(vault), &data_root) {
                Ok(mut opened) => {
                    apply_bundled_cli(&mut opened);
                    service = Some(std::sync::Arc::new(opened));
                }
                Err(error) => open_error = Some(error),
            }
        }
        let host = Self {
            data_root,
            clock,
            ui,
            sink,
            inner: Mutex::new(Inner {
                config,
                preferences,
                service,
                open_error,
                tokens: TokenStore::default(),
                journal: BTreeMap::new(),
                notices: BTreeMap::new(),
                notice_dedupe: BTreeSet::new(),
                progress_marks: None,
                navigation: Navigation {
                    epoch: ulid::Ulid::generate().to_string(),
                    next_sequence: 0,
                    focus: None,
                },
                media_tokens: BTreeMap::new(),
                requests: BTreeMap::new(),
                epoch: ulid::Ulid::generate().to_string(),
                seen_events: BTreeSet::new(),
                previous_state: None,
                unblocked: BTreeSet::new(),
                releases: BTreeMap::new(),
                hydrated: false,
            }),
            shutdown: AtomicBool::new(false),
            usage: Mutex::new(agendar_core::runtime::usage::UsageScanner::default()),
        };
        if host.notifications_consented() {
            host.sink.start();
        }
        Ok(host)
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The local config is shared with the CLI, which links folders on its own
    /// (`agendar project link`). Re-read it before using it, so revision
    /// guards, presence by folder and token lookups never act on a stale copy.
    fn reload_config(&self, inner: &mut Inner) {
        if let Ok(config) = Config::load_or_default(&self.data_root) {
            inner.config = config;
        }
    }

    /// Publish one committed change: the `agendar://changed` batch, then any
    /// `agendar://unblock` transitions it implies, each exactly once.
    fn publish(
        &self,
        inner: &mut Inner,
        state: &State,
        events: &[Event],
        generation: u64,
        now: UtcTime,
        hint: &str,
    ) -> Value {
        // Derive the release diff before `previous_state` moves forward.
        let unblocks = unblock_events(inner, state);
        let changed = changed_event(inner, state, events, generation, now, hint);
        remember(inner, state, events, false);
        self.ui.emit_changed(&changed);
        self.ui.attention(
            needs_count(state, &inner.preferences, now),
            working_count(state),
        );
        for unblock in unblocks {
            self.ui.emit_unblock(&unblock);
        }
        changed
    }

    /// Initial hydration: remember what exists so nothing old animates.
    fn hydrate(&self, inner: &mut Inner, state: &State, events: &[Event]) {
        remember(inner, state, events, true);
        let now = self.clock.now();
        self.ui.attention(
            needs_count(state, &inner.preferences, now),
            working_count(state),
        );
    }

    pub fn cancel(&self) {
        self.shutdown.store(true, Ordering::Release);
        let mut inner = self.lock();
        inner.tokens.clear();
        inner.media_tokens.clear();
        self.sink.shutdown();
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    pub fn notifications_consented(&self) -> bool {
        self.lock().preferences.notifications
    }

    // -----------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------

    fn fresh_state(&self, inner: &Inner, now: UtcTime) -> ApiResult<(State, u64)> {
        let service = inner
            .service
            .as_ref()
            .ok_or_else(|| not_configured(inner))?;
        let fresh = service.refresh(now, false).map_err(AppError::from)?;
        Ok((fresh.state, fresh.generation))
    }

    /// One converged read that keeps the open index, for local extras such as
    /// the activity feed that read `raw_events` directly.
    fn fresh_read(&self, inner: &Inner, now: UtcTime) -> ApiResult<Fresh> {
        let service = inner
            .service
            .as_ref()
            .ok_or_else(|| not_configured(inner))?;
        service.refresh(now, false).map_err(AppError::from)
    }

    /// One converged read that also keeps the event set, for projections that
    /// need event timestamps (the Brief's Delivered rows).
    fn fresh_state_with_events(
        &self,
        inner: &Inner,
        now: UtcTime,
    ) -> ApiResult<(State, Vec<agendar_core::model::Event>, u64)> {
        let service = inner
            .service
            .as_ref()
            .ok_or_else(|| not_configured(inner))?;
        let fresh = service.refresh(now, false).map_err(AppError::from)?;
        Ok((fresh.state, fresh.events, fresh.generation))
    }

    /// Local check proposals keyed by delivery id. They are local metadata,
    /// never a Vault event; a read failure simply omits them.
    fn local_proposals(&self, inner: &Inner) -> BTreeMap<String, Vec<Value>> {
        inner
            .service
            .as_ref()
            .and_then(|service| check_runtime::proposals(service.local(), service.vault()).ok())
            .unwrap_or_default()
    }

    pub fn get_snapshot(&self) -> ApiResult<Value> {
        let mut inner = self.lock();
        self.reload_config(&mut inner);
        let now = self.clock.now();
        let Fresh {
            index,
            state,
            events,
            generation,
            ..
        } = self.fresh_read(&inner, now)?;
        let waiting = inner
            .service
            .as_ref()
            .map(|service| service.waiting_asks(now))
            .unwrap_or_default();
        let tokens = self.tokens_lookup(&inner.config, now);
        let activity = project_extras_all(&index, &state, now, &waiting, &tokens);
        drop(index);
        inner.open_error = None;
        // Hydration never animates: only the first read seeds the current facts.
        if !inner.hydrated {
            self.hydrate(&mut inner, &state, &events);
            inner.hydrated = true;
        }
        self.reconcile_notifications(&mut inner, &state, now);
        let needs = projection::all_needs(&state);
        let brief = projection::brief(&state, &events, inner.preferences.last_looked_at, now);
        let mut state_value = projection::state_value(&state)?;
        projection::attach_check_proposals(&mut state_value, &self.local_proposals(&inner));
        projection::attach_activity(&mut state_value, &activity);
        let local = self.local_status(&inner, now);
        let presence = self.presence_rows(&inner, now);
        let preferences = inner.preferences.view();
        let navigation_epoch = inner.navigation.epoch.clone();
        let epoch = inner.epoch.clone();
        let notification_focus = inner.navigation.focus.clone();
        let mut snapshot = json!({
            "v": 1,
            "epoch": epoch,
            "navigation_epoch": navigation_epoch,
            "generation": generation,
            "state": state_value,
            "needs": needs,
            "brief": brief,
            "local": local,
            "presence": presence,
            "preferences": preferences,
        });
        if let Some(focus) = notification_focus
            && let Value::Object(object) = &mut snapshot
        {
            object.insert("notification_focus".to_owned(), focus);
        }
        Ok(snapshot)
    }

    pub fn get_project(&self, request: &Value) -> ApiResult<Value> {
        let project =
            ProjectSlug::parse(required_str(request, "project")?).map_err(AppError::from)?;
        let inner = self.lock();
        let now = self.clock.now();
        let fresh = self.fresh_read(&inner, now)?;
        let project_state = fresh.state.projects.get(&project).ok_or_else(|| {
            AppError::new(ErrorCode::NotFound.as_str(), "Project was not found.")
                .with_hint("Check the project slug.")
        })?;
        let mut project_value = projection::project_value(project_state)?;
        projection::attach_project_check_proposals(
            &mut project_value,
            &self.local_proposals(&inner),
        );
        let waiting = inner
            .service
            .as_ref()
            .map(|service| service.waiting_asks(now))
            .unwrap_or_default();
        let tokens = self.tokens_lookup(&inner.config, now)(&project);
        let extras = project_extras(&fresh.index, project_state, now, &waiting, tokens);
        projection::attach_project_activity(&mut project_value, &extras);
        Ok(json!({
            "project": project_value,
            "graph": projection::graph(project_state),
        }))
    }

    pub fn search(&self, request: &Value) -> ApiResult<Value> {
        let text = request
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let inner = self.lock();
        let now = self.clock.now();
        let (state, _) = self.fresh_state(&inner, now)?;
        let mut needs = Vec::new();
        for (slug, project) in &state.projects {
            let policy = project.policy.revision.clone();
            for need in &project.needs_you {
                if projection::matches(
                    text,
                    &[
                        need.question.as_str(),
                        slug.as_str(),
                        need.node.as_ref().map_or("", |node| node.as_str()),
                    ],
                ) {
                    let decision = project.decisions.get(&need.source);
                    let mut value = serde_json::to_value(need).unwrap_or(Value::Null);
                    if let Value::Object(object) = &mut value {
                        object.insert(
                            "revision".to_owned(),
                            json!(projection::need_revision(need, decision, &policy)),
                        );
                    }
                    needs.push(value);
                }
            }
        }
        needs.sort_by(|left, right| {
            right
                .get("score_x2")
                .and_then(Value::as_u64)
                .cmp(&left.get("score_x2").and_then(Value::as_u64))
        });
        let projects: Vec<Value> = state
            .projects
            .values()
            .filter(|project| {
                projection::matches(
                    text,
                    &[
                        project.summary.slug.as_str(),
                        project.summary.title.as_str(),
                        project.summary.brief.as_str(),
                    ],
                )
            })
            .filter_map(|project| serde_json::to_value(&project.summary).ok())
            .collect();
        Ok(json!({"needs": needs, "projects": projects}))
    }

    fn local_status(&self, inner: &Inner, now: UtcTime) -> Value {
        let host = inner
            .service
            .as_ref()
            .map(|service| service.host().as_str().to_owned())
            .unwrap_or_default();
        let mut capabilities = Vec::new();
        if let Some(service) = inner.service.as_ref() {
            for harness in [Harness::ClaudeCode, Harness::Generic] {
                let capability = service.capabilities_for(harness);
                capabilities.push(json!({
                    "harness": harness,
                    "running": capability.running,
                    "ended_turn": capability.ended_turn,
                    "new_session": capability.new_session,
                    "queue_supported": capability.queue_supported,
                    "automatic_wake": capability.automatic_wake,
                    "detail": capability.detail,
                }));
            }
        }
        let consented = inner.preferences.notifications;
        let capability = if consented {
            self.sink.capability()
        } else {
            Availability::Unknown
        };
        let notifications = if !consented {
            "unknown"
        } else {
            match capability {
                Availability::Available => "granted",
                Availability::Unavailable => {
                    if self
                        .sink
                        .reason()
                        .is_some_and(|reason| reason.contains("denied"))
                    {
                        "denied"
                    } else {
                        "unknown"
                    }
                }
                Availability::Unknown => "unknown",
            }
        };
        let mut actions = json!({"availability": capability.as_str()});
        if let Some(reason) = self.sink.reason()
            && let Value::Object(object) = &mut actions
        {
            object.insert("reason".to_owned(), json!(reason));
        }
        let mut status = json!({
            "host": host,
            "received_at": now,
            "health": "healthy",
            "diagnostics": [],
            "capabilities": capabilities,
            "notifications": notifications,
            "notification_actions": actions,
        });
        // What the platform said when it refused the last notice (macOS says
        // so only after `show` returned).
        if let Some(error) = self.sink.last_error()
            && let Value::Object(object) = &mut status
        {
            object.insert("notification_error".to_owned(), json!(error));
        }
        status
    }

    /// Show one Open-only test notice now, whatever the consent setting, and
    /// How each agent harness on this computer is bound to Agendar (hooks or
    /// the MCP server), for Settings › Agents.
    pub fn bindings(&self) -> ApiResult<Value> {
        let cli = bundled_cli_path()?;
        let home = agendar_core::runtime::install::home_dir().map_err(AppError::from)?;
        let seen = self.last_seen_by_harness();
        let mut bindings =
            serde_json::to_value(agendar_core::runtime::install::bindings(&cli, &home))
                .map_err(|error| AppError::new(ErrorCode::Io.as_str(), error.to_string()))?;
        // An agent can use Agendar through the skill and the CLI without hooks
        // or an MCP entry; when one reported from this computer, say so.
        if let Value::Array(rows) = &mut bindings {
            for row in rows {
                let harness = row
                    .get("harness")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let (Some(harness), Value::Object(object)) = (harness, row)
                    && let Some(ts) = seen.get(&harness)
                {
                    object.insert("last_seen".to_owned(), json!(ts));
                }
            }
        }
        Ok(json!({
            "bindings": bindings,
            "cli_path": cli.to_string_lossy(),
        }))
    }

    /// The newest event time of any session on this computer, per harness
    /// (its wire name), for Settings › Agents.
    pub fn last_seen_by_harness(&self) -> BTreeMap<String, UtcTime> {
        let inner = self.lock();
        let Some(service) = inner.service.as_ref() else {
            return BTreeMap::new();
        };
        let Ok(fresh) = service.refresh(self.clock.now(), true) else {
            return BTreeMap::new();
        };
        let host = service.host().clone();
        let mut seen: BTreeMap<String, UtcTime> = BTreeMap::new();
        for session in fresh
            .state
            .projects
            .values()
            .flat_map(|project| project.sessions.values())
            .filter(|session| session.host == host)
        {
            let Some(harness) = serde_json::to_value(session.harness)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
            else {
                continue;
            };
            let newer = seen
                .get(&harness)
                .is_none_or(|known| session.last_event_ts > *known);
            if newer {
                seen.insert(harness, session.last_event_ts);
            }
        }
        seen
    }

    /// Bind or unbind one harness from Settings (the person's explicit click).
    pub fn set_binding(&self, request: &Value) -> ApiResult<Value> {
        let harness: agendar_core::model::Harness = serde_json::from_value(
            request
                .get("harness")
                .cloned()
                .ok_or_else(|| AppError::validation("/harness", "harness is required."))?,
        )
        .map_err(|error| AppError::validation("/harness", format!("Unknown harness: {error}.")))?;
        let action = required_str(request, "action")?;
        let cli = bundled_cli_path()?;
        let home = agendar_core::runtime::install::home_dir().map_err(AppError::from)?;
        let binding = match action {
            "bind" => agendar_core::runtime::install::bind(harness, &cli, &home),
            "unbind" => agendar_core::runtime::install::unbind(harness, &cli, &home),
            _ => {
                return Err(AppError::validation(
                    "/action",
                    "action must be bind or unbind.",
                ));
            }
        }
        .map_err(AppError::from)?;
        Ok(json!({"binding": binding}))
    }

    /// re-read the platform permission. A refusal macOS reports later appears
    /// as `local.notification_error` in the next snapshot.
    pub fn test_notice(&self) -> ApiResult<Value> {
        self.sink.refresh();
        let spec = NoticeSpec {
            id: new_notice_id(),
            title: "Agendar".to_owned(),
            body: "Test notification: if you can see this, notifications work.".to_owned(),
            choices: Vec::new(),
            project: String::new(),
            item_id: "test".to_owned(),
        };
        Ok(match self.sink.show(&spec) {
            Ok(()) => json!({"sent": true}),
            Err(error) => json!({"sent": false, "error": error}),
        })
    }

    // -----------------------------------------------------------------
    // Prepare and answer
    // -----------------------------------------------------------------

    pub fn prepare_answer(&self, surface: &str, request: &Value) -> ApiResult<Value> {
        let ask_id = EventId::parse(required_str(request, "ask_id")?).map_err(AppError::from)?;
        let option_id =
            OptionId::parse(required_str(request, "option_id")?).map_err(AppError::from)?;
        let expected_revision = Sha256::parse(required_str(request, "expected_decision_revision")?)
            .map_err(AppError::from)?;
        let expected_receipt = optional_str(request, "expected_receipt")
            .map(EventId::parse)
            .transpose()
            .map_err(AppError::from)?;

        let mut inner = self.lock();
        let now = self.clock.now();
        let (state, _) = self.fresh_state(&inner, now)?;
        let (project, decision) = find_decision(&state, &ask_id).ok_or_else(|| {
            AppError::new(ErrorCode::NotFound.as_str(), "Decision was not found.")
                .with_hint("Refresh the board.")
        })?;
        if decision.lifecycle != DecisionLifecycle::Open {
            return Err(AppError::new(
                ErrorCode::StaleAnswer.as_str(),
                "This decision was already answered or closed.",
            )
            .with_hint("Refresh the current card."));
        }
        let revision = projection::decision_revision(
            project,
            decision,
            &state.projects[project].policy.revision,
        );
        if revision != expected_revision {
            return Err(stale_answer());
        }
        if decision.effective_receipt != expected_receipt {
            return Err(stale_answer());
        }
        // An answer in the human's own words: main window only, never a
        // one-key or notification path.
        let own_answer = if option_id.as_str() == agendar_core::model::OWN_OPTION {
            if surface != "main" {
                return Err(AppError::forbidden(
                    "An answer in your own words is written in the main window.",
                ));
            }
            let text = optional_str(request, "own_answer")
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| AppError::validation("/own_answer", "Write your answer first."))?;
            if text.chars().count() > agendar_core::model::OWN_ANSWER_MAX {
                return Err(AppError::validation(
                    "/own_answer",
                    format!(
                        "Keep your answer within {} characters.",
                        agendar_core::model::OWN_ANSWER_MAX
                    ),
                ));
            }
            Some(text.to_owned())
        } else {
            None
        };
        let chosen = match &own_answer {
            Some(text) => decision.card.own_option(text),
            None => decision
                .card
                .options
                .iter()
                .find(|option| option.id == option_id)
                .cloned()
                .ok_or_else(|| {
                    AppError::validation(
                        "/option_id",
                        "The option does not belong to this decision.",
                    )
                })?,
        };
        let risk = effective_risk(&decision.card);
        let guard = guard::guard_for(&decision.card);
        if surface == "popover"
            && (guard != Guard::Easy
                || chosen.id != decision.card.recommended.option
                || !matches!(
                    decision.card.recommended.confidence,
                    Confidence::Med | Confidence::High
                ))
        {
            return Err(AppError::forbidden(
                "The popover may only prepare the current easy med/high recommendation.",
            ));
        }
        let recommended = chosen.id == decision.card.recommended.option
            && matches!(
                decision.card.recommended.confidence,
                Confidence::Med | Confidence::High
            );
        // A main-window alternative on an easy card is a deliberate explicit
        // choice. The notification and popover surfaces stay
        // recommendation-only (the popover check above and the notification
        // eligibility matrix never prepare an alternative).
        let explicit_choice = (guard == Guard::Easy && !recommended) || own_answer.is_some();
        let receipt = receipt_preview(decision, &chosen);
        let token = new_token();
        let expires_at = now.add_millis(guard::TOKEN_TTL_S * 1_000);
        inner.tokens.insert(
            token.clone(),
            PreparedToken {
                surface: surface.to_owned(),
                ask: ask_id,
                option: chosen.id.clone(),
                revision,
                receipt: decision.effective_receipt.clone(),
                label: chosen.label.clone(),
                risk,
                guard,
                explicit_choice,
                expires_at,
                used: false,
                notice_id: None,
                request_id: None,
                own_answer,
            },
        );
        Ok(guard::prepare_data(
            token,
            expires_at,
            risk,
            chosen.label,
            receipt,
            guard,
        ))
    }

    pub fn answer_decision(&self, surface: &str, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?;
        EventId::parse(request_id).map_err(AppError::from)?; // canonical ULID
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let token = required_str(request, "token")?.to_owned();
        let echo = required_str(request, "echo_label")?.to_owned();
        let done_when = parse_done_when(request.get("done_when"))?;
        let expected_eta = parse_eta_field(request.get("expected_eta"))?;

        let mut inner = self.lock();
        if let Some(entry) = inner.journal.get(request_id) {
            if entry.command == "answer_decision" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        let data =
            self.commit_answer(&mut inner, surface, &token, &echo, done_when, expected_eta)?;
        inner.journal.insert(
            request_id.to_owned(),
            JournalEntry {
                command: "answer_decision".to_owned(),
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    fn commit_answer(
        &self,
        inner: &mut Inner,
        surface: &str,
        token_id: &str,
        echo: &str,
        done_when: Option<Vec<agendar_core::model::Criterion>>,
        expected_eta: Option<agendar_core::model::EtaRange>,
    ) -> ApiResult<Value> {
        let now = self.clock.now();
        let token = inner
            .tokens
            .get(token_id)
            .cloned()
            .ok_or_else(stale_answer)?;
        if token.used || token.surface != surface || token.is_expired(now) {
            return Err(stale_answer());
        }
        let (state, _) = self.fresh_state(inner, now)?;
        let (project, decision) = find_decision(&state, &token.ask).ok_or_else(stale_answer)?;
        let project = project.clone();
        let policy = state.projects[&project].policy.revision.clone();
        let revision = projection::decision_revision(&project, decision, &policy);
        if revision != token.revision || decide_receipt(decision) != token.receipt {
            return Err(stale_answer());
        }
        if decision.lifecycle != DecisionLifecycle::Open {
            return Err(AppError::new(
                ErrorCode::StaleAnswer.as_str(),
                "This decision is no longer open.",
            )
            .with_hint("Refresh the current card."));
        }
        let chosen = match &token.own_answer {
            Some(text) => decision.card.own_option(text),
            None => decision
                .card
                .options
                .iter()
                .find(|option| option.id == token.option)
                .cloned()
                .ok_or_else(stale_answer)?,
        };
        let current_guard = guard::guard_for(&decision.card);
        if current_guard != token.guard || effective_risk(&decision.card) != token.risk {
            return Err(stale_answer());
        }
        if echo.trim() != chosen.label.trim() {
            return Err(AppError::validation(
                "/echo_label",
                "The echoed label does not match the current chosen option.",
            )
            .with_hint("Review the refreshed card and choose again."));
        }
        match current_guard {
            Guard::Easy => {
                // Only the one-key path is recommendation-only. A token
                // prepared for a main-window explicit alternative carries that
                // classification into the commit, so a deliberately chosen
                // other option still answers the human's real question.
                if !token.explicit_choice
                    && (chosen.id != decision.card.recommended.option
                        || !matches!(
                            decision.card.recommended.confidence,
                            Confidence::Med | Confidence::High
                        ))
                {
                    return Err(AppError::new(
                        ErrorCode::Validation.as_str(),
                        "This decision needs an explicit choice; the recommendation is not eligible for one-key approval.",
                    )
                    .with_hint("Open Agendar and choose an option."));
                }
            }
            Guard::Confirm | Guard::Choose => {
                if echo.trim().is_empty() {
                    return Err(AppError::validation(
                        "/echo_label",
                        "Echo the chosen option label before answering.",
                    ));
                }
            }
        }
        let before = state.projects[&project].clone();
        let service = inner
            .service
            .as_ref()
            .ok_or_else(|| not_configured(inner))?;
        let event_id = match &token.own_answer {
            Some(text) => {
                service.answer_own(&project, &token.ask, text.clone(), done_when, expected_eta)
            }
            None => service.answer_decision(
                &project,
                &token.ask,
                &token.option,
                done_when,
                expected_eta,
                None,
            ),
        }
        .map_err(AppError::from)?;
        // Re-evaluate after the append with the current clock: the event's own
        // timestamp may be a millisecond later than the pre-append read.
        let after_now = self.clock.now();
        let (after_state, events, generation) = self.fresh_state_with_events(inner, after_now)?;
        let after = after_state
            .projects
            .get(&project)
            .ok_or_else(|| AppError::new(ErrorCode::NotFound.as_str(), "Project was not found."))?;
        let receipt = after
            .decisions
            .get(&token.ask)
            .and_then(|decision| decision.effective_receipt.clone())
            .and_then(|receipt| after.receipts.get(&receipt).cloned())
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::Io.as_str(),
                    "The answer was committed but its receipt is not in the current projection.",
                )
                .retryable(true)
            })?;
        let released = released_nodes(&before, after, &decision.node);
        if let Some(stored) = inner.tokens.get_mut(token_id) {
            stored.used = true;
        }
        for binding in inner.notices.values_mut() {
            if binding
                .choices
                .iter()
                .any(|choice| choice.token == token_id)
            {
                binding.answered = true;
            }
        }
        let data = json!({
            "event_id": event_id,
            "receipt": receipt,
            "awaiting_ack": true,
            "released": released,
        });
        // Emit after the state is committed. UI calls never write the Vault.
        self.publish(inner, &after_state, &events, generation, after_now, "local");
        if inner.preferences.notifications {
            self.reconcile_notifications(inner, &after_state, after_now);
        }
        Ok(data)
    }

    // -----------------------------------------------------------------
    // Delivery review, house rules, reports and approved checks
    // -----------------------------------------------------------------

    /// Accept or send back one delivery. The caller echoes the delivery revision
    /// it rendered; a changed observation or review frontier is stale.
    pub fn review_delivery(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?.to_owned();
        EventId::parse(&request_id).map_err(AppError::from)?;
        let delivery_id =
            EventId::parse(required_str(request, "delivery_id")?).map_err(AppError::from)?;
        let expected = Sha256::parse(required_str(request, "expected_delivery_revision")?)
            .map_err(AppError::from)?;
        let verdict = match required_str(request, "verdict")? {
            "accept" => ReviewVerdict::Accept,
            "send_back" => ReviewVerdict::SendBack,
            other => {
                return Err(AppError::validation(
                    "/verdict",
                    format!("Unknown review verdict `{other}`; use accept or send_back."),
                ));
            }
        };
        let reason = optional_str(request, "reason")
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(str::to_owned);
        if verdict == ReviewVerdict::SendBack && reason.is_none() {
            return Err(AppError::validation(
                "/reason",
                "Sending a delivery back requires a reason.",
            ));
        }
        let save_rule = parse_save_rule(request.get("save_rule"))?;
        if save_rule.is_some() && verdict != ReviewVerdict::SendBack {
            return Err(AppError::validation(
                "/save_rule",
                "save_rule is only allowed when sending a delivery back.",
            ));
        }
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(&request_id) {
            if entry.command == "review_delivery" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        let now = self.clock.now();
        let (state, _) = self.fresh_state(&inner, now)?;
        let (slug, project, delivery) = find_delivery(&state, &delivery_id).ok_or_else(|| {
            AppError::new(ErrorCode::NotFound.as_str(), "Delivery was not found.")
                .with_hint("Refresh the project board.")
        })?;
        let slug = slug.clone();
        let current_receipt = current_receipt(project, delivery);
        if projection::delivery_revision(&slug, delivery, current_receipt) != expected {
            return Err(AppError::new(
                ErrorCode::StaleAnswer.as_str(),
                "The delivery changed after it was displayed.",
            )
            .with_hint("Review the refreshed observation and review frontier."));
        }
        if delivery.superseded_by.is_some() {
            return Err(AppError::new(
                ErrorCode::StaleAnswer.as_str(),
                "This delivery was superseded by a newer one.",
            )
            .with_hint("Review the current delivery."));
        }
        if current_receipt != Some(&delivery.receipt) {
            return Err(AppError::new(
                ErrorCode::StaleAnswer.as_str(),
                "This delivery's receipt is no longer current.",
            )
            .with_hint("Review the delivery against the current receipt."));
        }
        let node = delivery.node.clone();
        let mut causes: BTreeSet<EventId> = BTreeSet::new();
        causes.insert(delivery.receipt.clone());
        if let Some(receipt) = current_receipt {
            causes.insert(receipt.clone());
        }
        for criterion in &delivery.criteria {
            if let Some(observation) = &criterion.observation {
                causes.insert(observation.clone());
            }
            causes.extend(criterion.observation_frontier.iter().cloned());
        }
        causes.extend(delivery.review_frontier.iter().cloned());
        let before = project.clone();
        let service = inner
            .service
            .as_ref()
            .ok_or_else(|| not_configured(&inner))?;
        let (event_id, rule_event) = service
            .review_delivery(
                &slug,
                &node,
                &delivery_id,
                verdict,
                reason,
                save_rule,
                causes.into_iter().collect(),
            )
            .map_err(AppError::from)?;
        let after_now = self.clock.now();
        let (after_state, events, generation) = self.fresh_state_with_events(&inner, after_now)?;
        let after = after_state
            .projects
            .get(&slug)
            .ok_or_else(|| AppError::new(ErrorCode::NotFound.as_str(), "Project was not found."))?;
        let released = released_nodes(&before, after, &node);
        let mut data = json!({
            "event_id": event_id,
            "released": released,
        });
        if let Some(rule_event) = rule_event
            && let Value::Object(object) = &mut data
        {
            object.insert("rule_event".to_owned(), json!(rule_event));
        }
        self.publish(
            &mut inner,
            &after_state,
            &events,
            generation,
            after_now,
            "local",
        );
        inner.requests.insert(
            request_id,
            RequestEntry {
                command: "review_delivery",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    /// Set one house rule for the project (human-authored operator event).
    pub fn set_rule(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?.to_owned();
        EventId::parse(&request_id).map_err(AppError::from)?;
        let project =
            ProjectSlug::parse(required_str(request, "project")?).map_err(AppError::from)?;
        let expected =
            Sha256::parse(required_str(request, "expected_revision")?).map_err(AppError::from)?;
        let rule = parse_rule(request.get("rule"))?;
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(&request_id) {
            if entry.command == "set_rule" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        let now = self.clock.now();
        let (state, generation) = self.fresh_state(&inner, now)?;
        let project_state = state.projects.get(&project).ok_or_else(|| {
            AppError::new(ErrorCode::NotFound.as_str(), "Project was not found.")
                .with_hint("Check the project slug.")
        })?;
        let current = project_state
            .rules
            .iter()
            .find(|state| state.value.rule_id == rule.rule_id)
            .map(|state| state.revision.clone())
            .unwrap_or_else(|| agendar_core::model::rule_revision(&project, &rule.rule_id, None));
        if current != expected {
            return Err(stale_answer());
        }
        let service = inner
            .service
            .as_ref()
            .ok_or_else(|| not_configured(&inner))?;
        let event_id = service
            .set_rule(&project, rule, Vec::new())
            .map_err(AppError::from)?;
        let after_now = self.clock.now();
        let (after_state, events, after_generation) =
            self.fresh_state_with_events(&inner, after_now)?;
        let data = json!({"event_id": event_id});
        self.publish(
            &mut inner,
            &after_state,
            &events,
            after_generation.max(generation),
            after_now,
            "local",
        );
        inner.requests.insert(
            request_id,
            RequestEntry {
                command: "set_rule",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    /// `set_policy`: presets, cells and budget numbers through the operator
    /// writer. The backend re-validates the locked irreversible cells.
    pub fn set_policy(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?.to_owned();
        EventId::parse(&request_id).map_err(AppError::from)?;
        let project =
            ProjectSlug::parse(required_str(request, "project")?).map_err(AppError::from)?;
        let expected =
            Sha256::parse(required_str(request, "expected_revision")?).map_err(AppError::from)?;
        let policy: PolicySet = serde_json::from_value(
            request
                .get("policy")
                .cloned()
                .ok_or_else(|| AppError::validation("/policy", "policy is required."))?,
        )
        .map_err(|error| AppError::validation("/policy", format!("Invalid policy: {error}.")))?;
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(&request_id) {
            if entry.command == "set_policy" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        let service = inner
            .service
            .as_ref()
            .ok_or_else(|| not_configured(&inner))?;
        let event_id = service
            .set_policy(&project, policy, &expected)
            .map_err(AppError::from)?;
        let after_now = self.clock.now();
        let (after_state, events, after_generation) =
            self.fresh_state_with_events(&inner, after_now)?;
        let data = json!({"event_id": event_id});
        self.publish(
            &mut inner,
            &after_state,
            &events,
            after_generation,
            after_now,
            "local",
        );
        inner.requests.insert(
            request_id,
            RequestEntry {
                command: "set_policy",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    /// `update_project`: human-owned project fields with per-field guards.
    pub fn update_project(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?.to_owned();
        EventId::parse(&request_id).map_err(AppError::from)?;
        let project =
            ProjectSlug::parse(required_str(request, "project")?).map_err(AppError::from)?;
        let expected = parse_expected_fields(request.get("expected_fields"))?;
        let patch: ProjectDeclared = serde_json::from_value(
            request
                .get("patch")
                .cloned()
                .ok_or_else(|| AppError::validation("/patch", "patch is required."))?,
        )
        .map_err(|error| AppError::validation("/patch", format!("Invalid patch: {error}.")))?;
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(&request_id) {
            if entry.command == "update_project" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        let service = inner
            .service
            .as_ref()
            .ok_or_else(|| not_configured(&inner))?;
        let event_id = service
            .update_project(&project, patch, &expected)
            .map_err(AppError::from)?;
        let after_now = self.clock.now();
        let (after_state, events, after_generation) =
            self.fresh_state_with_events(&inner, after_now)?;
        let data = json!({"event_id": event_id});
        self.publish(
            &mut inner,
            &after_state,
            &events,
            after_generation,
            after_now,
            "local",
        );
        inner.requests.insert(
            request_id,
            RequestEntry {
                command: "update_project",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    /// `update_feature`: human-owned agenda fields with per-field and graph
    /// guards. Cycle-checked before any append.
    pub fn update_feature(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?.to_owned();
        EventId::parse(&request_id).map_err(AppError::from)?;
        let project =
            ProjectSlug::parse(required_str(request, "project")?).map_err(AppError::from)?;
        let node = agendar_core::model::NodePath::parse(required_str(request, "node")?)
            .map_err(AppError::from)?;
        let expected = parse_expected_fields(request.get("expected_fields"))?;
        let expected_graph = optional_str(request, "expected_graph_revision")
            .map(Sha256::parse)
            .transpose()
            .map_err(AppError::from)?;
        let patch: PlanFeature = serde_json::from_value(
            request
                .get("patch")
                .cloned()
                .ok_or_else(|| AppError::validation("/patch", "patch is required."))?,
        )
        .map_err(|error| AppError::validation("/patch", format!("Invalid patch: {error}.")))?;
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(&request_id) {
            if entry.command == "update_feature" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        let service = inner
            .service
            .as_ref()
            .ok_or_else(|| not_configured(&inner))?;
        let event_id = service
            .update_feature(&project, &node, patch, &expected, expected_graph.as_ref())
            .map_err(AppError::from)?;
        let after_now = self.clock.now();
        let (after_state, events, after_generation) =
            self.fresh_state_with_events(&inner, after_now)?;
        let data = json!({"event_id": event_id});
        self.publish(
            &mut inner,
            &after_state,
            &events,
            after_generation,
            after_now,
            "local",
        );
        inner.requests.insert(
            request_id,
            RequestEntry {
                command: "update_feature",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    /// Serve one report as text (escaped by the frontend) or an expiring media
    /// capability. No report bytes are rendered by the backend.
    pub fn read_report(&self, request: &Value) -> ApiResult<Value> {
        let project =
            ProjectSlug::parse(required_str(request, "project")?).map_err(AppError::from)?;
        let blob: BlobRef = serde_json::from_value(
            request
                .get("blob")
                .cloned()
                .ok_or_else(|| AppError::validation("/blob", "blob is required."))?,
        )
        .map_err(|error| AppError::validation("/blob", format!("Invalid blob: {error}.")))?;
        blob.validate("/blob").map_err(AppError::from)?;
        let mut inner = self.lock();
        let now = self.clock.now();
        let fresh = {
            let service = inner
                .service
                .as_ref()
                .ok_or_else(|| not_configured(&inner))?;
            service.refresh(now, false).map_err(AppError::from)?
        };
        if !fresh.state.projects.contains_key(&project) {
            return Err(AppError::new(
                ErrorCode::NotFound.as_str(),
                "Project was not found.",
            ));
        }
        if !agendar_core::reduce::evidence::blob_referenced(&fresh.events, &project, &blob, true) {
            return Err(AppError::new(
                ErrorCode::NotFound.as_str(),
                "This blob is not referenced by the project.",
            )
            .with_hint("Open the decision or delivery that published it."));
        }
        let media_referenced =
            agendar_core::reduce::evidence::blob_referenced(&fresh.events, &project, &blob, false);
        let service = inner
            .service
            .as_ref()
            .ok_or_else(|| not_configured(&inner))?;
        let bytes = read_blob(service.vault(), &project, &blob).map_err(AppError::from)?;
        let mime = mime_for(blob.ext).ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported.as_str(),
                "This report type is not supported in this version.",
            )
            .with_hint("Text, Markdown, HTML and image reports are supported.")
        })?;
        if is_text(blob.ext) {
            let text = String::from_utf8(bytes)
                .map_err(|_| AppError::validation("/blob", "A text report must be valid UTF-8."))?;
            return Ok(json!({
                "mime": mime,
                "text": text,
                "sha256": blob.sha256,
            }));
        }
        // SVG is served only as a project icon (and the media route adds a
        // sandbox CSP), never as evidence or an option image.
        let svg_icon = blob.ext == BlobExt::Svg
            && agendar_core::reduce::evidence::project_icon_referenced(
                &fresh.events,
                &project,
                &blob,
            );
        if is_media(blob.ext) || svg_icon {
            if !media_referenced {
                return Err(AppError::new(
                    ErrorCode::Unsupported.as_str(),
                    "Media is only served for evidence and decision option images.",
                )
                .with_hint("Open the report as source text instead of media."));
            }
            let token = guard::new_token();
            let expires_at = now.add_millis(MEDIA_TOKEN_TTL_S * 1_000);
            let sha256 = blob.sha256.clone();
            inner.media_tokens.insert(
                token.clone(),
                MediaToken {
                    project,
                    blob,
                    mime: mime.to_owned(),
                    expires_at,
                },
            );
            return Ok(json!({
                "mime": mime,
                "media_token": token,
                "sha256": sha256,
            }));
        }
        Err(AppError::new(
            ErrorCode::Unsupported.as_str(),
            "This report type is not supported in this version.",
        ))
    }

    /// Validate and persist one explicit human approval of an exact check
    /// profile. The command never runs the check.
    pub fn approve_check(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?.to_owned();
        EventId::parse(&request_id).map_err(AppError::from)?;
        let expected = Sha256::parse(required_str(request, "expected_approval_revision")?)
            .map_err(AppError::from)?;
        if request.get("consent").and_then(Value::as_bool) != Some(true) {
            return Err(AppError::new(
                ErrorCode::Consent.as_str(),
                "Explicit consent is required before approving a check.",
            )
            .with_hint("Show the exact argv, working directory and timeout first."));
        }
        let profile: CheckProfile = serde_json::from_value(
            request
                .get("profile")
                .cloned()
                .ok_or_else(|| AppError::validation("/profile", "profile is required."))?,
        )
        .map_err(|error| AppError::validation("/profile", format!("Invalid profile: {error}.")))?;
        let normalized = profile.normalize().map_err(AppError::from)?;
        let absent = check_runtime::absent_revision(&normalized);
        let digest = check_runtime::profile_digest(&normalized);
        if expected != absent && expected != digest {
            return Err(stale_answer());
        }
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let payload_digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(&request_id) {
            if entry.command == "approve_check" && entry.digest == payload_digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        let service = inner
            .service
            .as_ref()
            .ok_or_else(|| not_configured(&inner))?;
        let config = Config::load_or_default(&self.data_root).map_err(AppError::from)?;
        let approval = check_runtime::approve(
            service.local(),
            service.vault(),
            &config,
            &profile,
            self.clock.now(),
        )
        .map_err(AppError::from)?;
        let data = json!({"approval_id": approval.id.as_str()});
        inner.requests.insert(
            request_id,
            RequestEntry {
                command: "approve_check",
                digest: payload_digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    /// Run one approved check and publish `evidence.observed` only if the
    /// delivery revision is still the one the human reviewed.
    pub fn run_check(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?.to_owned();
        EventId::parse(&request_id).map_err(AppError::from)?;
        let project =
            ProjectSlug::parse(required_str(request, "project")?).map_err(AppError::from)?;
        let delivery_id =
            EventId::parse(required_str(request, "delivery_id")?).map_err(AppError::from)?;
        let criterion_id =
            CriterionId::parse(required_str(request, "criterion")?).map_err(AppError::from)?;
        let approval_id = required_str(request, "approval_id")?.to_owned();
        let expected_delivery = Sha256::parse(required_str(request, "expected_delivery_revision")?)
            .map_err(AppError::from)?;
        let expected_approval = Sha256::parse(required_str(request, "expected_approval_revision")?)
            .map_err(AppError::from)?;
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let (service, approval, node, frontier) = {
            let inner = self.lock();
            if let Some(entry) = inner.requests.get(&request_id) {
                if entry.command == "run_check" && entry.digest == digest {
                    return Ok(entry.data.clone());
                }
                return Err(request_conflict());
            }
            let now = self.clock.now();
            let (state, _) = self.fresh_state(&inner, now)?;
            let project_state = state.projects.get(&project).ok_or_else(|| {
                AppError::new(ErrorCode::NotFound.as_str(), "Project was not found.")
            })?;
            let delivery = project_state.deliveries.get(&delivery_id).ok_or_else(|| {
                AppError::new(ErrorCode::NotFound.as_str(), "Delivery was not found.")
            })?;
            let current_receipt = current_receipt(project_state, delivery);
            if projection::delivery_revision(&project, delivery, current_receipt)
                != expected_delivery
            {
                return Err(stale_answer());
            }
            let criterion = delivery
                .criteria
                .iter()
                .find(|criterion| criterion.criterion.id == criterion_id)
                .ok_or_else(|| {
                    AppError::validation(
                        "/criterion",
                        "The criterion does not belong to this delivery.",
                    )
                })?;
            let service = inner
                .service
                .as_ref()
                .ok_or_else(|| not_configured(&inner))?;
            let local = LocalPaths::new(&self.data_root).map_err(AppError::from)?;
            let approval = check_runtime::load(&local, service.vault(), &approval_id)
                .map_err(AppError::from)?
                .ok_or_else(|| {
                    AppError::new(
                        ErrorCode::NotFound.as_str(),
                        "The check approval was not found on this host.",
                    )
                    .with_hint("Approve the exact profile again.")
                })?;
            if expected_approval != approval.revision && expected_approval != approval.id {
                return Err(stale_answer());
            }
            if approval
                .profile
                .delivery_id
                .as_ref()
                .is_some_and(|id| id != &delivery_id)
                || approval
                    .profile
                    .criterion
                    .as_ref()
                    .is_some_and(|id| id != &criterion_id)
            {
                return Err(AppError::validation(
                    "/approval_id",
                    "This approval belongs to a different delivery or criterion.",
                ));
            }
            let config = Config::load_or_default(&self.data_root).map_err(AppError::from)?;
            check_runtime::validate_cwd(&approval.profile.cwd, &config, Some(project.as_str()))
                .map_err(AppError::from)?;
            (
                std::sync::Arc::clone(service),
                approval,
                delivery.node.clone(),
                criterion.observation_frontier.clone(),
            )
        };
        let local = LocalPaths::new(&self.data_root).map_err(AppError::from)?;
        let run = check_runtime::run(&local, service.vault(), &approval).map_err(AppError::from)?;
        // A failed/timeout check still records its bounded output; an empty
        // stream gets an explicit marker instead of an invalid zero-byte blob.
        let output = if run.output.is_empty() {
            format!(
                "check produced no output (timed_out={}:{})\n",
                run.timed_out,
                check_result_str(run.result)
            )
            .into_bytes()
        } else {
            run.output.clone()
        };
        let mut reader = output.as_slice();
        let output_blob = put_blob(
            service.vault(),
            service.local(),
            &project,
            service.host(),
            BlobExt::Txt,
            &mut reader,
        )
        .map_err(AppError::from)?;
        let mut inner = self.lock();
        let now = self.clock.now();
        let (state, generation) = self.fresh_state(&inner, now)?;
        let project_state = state
            .projects
            .get(&project)
            .ok_or_else(|| AppError::new(ErrorCode::NotFound.as_str(), "Project was not found."))?;
        let delivery = project_state.deliveries.get(&delivery_id).ok_or_else(|| {
            AppError::new(ErrorCode::NotFound.as_str(), "Delivery was not found.")
        })?;
        let current_receipt = current_receipt(project_state, delivery);
        if projection::delivery_revision(&project, delivery, current_receipt) != expected_delivery {
            return Err(AppError::new(
                ErrorCode::StaleAnswer.as_str(),
                "The delivery changed while the check was running; its result was not published.",
            )
            .with_hint("Review the current delivery and run an approved check again."));
        }
        let causes = if frontier.is_empty() {
            Vec::new()
        } else {
            frontier
        };
        let body = EvidenceObserved {
            criterion: criterion_id.clone(),
            method: ObservationMethod::Command {
                check_id: approval.id.as_str().to_owned(),
                argv_digest: approval.revision.clone(),
            },
            started_at: run.started_at,
            finished_at: run.finished_at,
            result: run.result,
            digest: output_blob.sha256.clone(),
            output: output_blob,
            approval_id: approval.id.as_str().to_owned(),
        };
        let observation_id = service
            .append_observation(&project, &node, &delivery_id, body, causes)
            .map_err(AppError::from)?;
        let data = json!({
            "observation_id": observation_id,
            "result": run.result,
        });
        let after_now = self.clock.now();
        let (after_state, events, after_generation) =
            self.fresh_state_with_events(&inner, after_now)?;
        self.publish(
            &mut inner,
            &after_state,
            &events,
            after_generation.max(generation),
            after_now,
            "local",
        );
        inner.requests.insert(
            request_id,
            RequestEntry {
                command: "run_check",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    /// Resolve one opaque media token for the `agendar-media` protocol. Only an
    /// exact token path is accepted; traversal, unknown/expired tokens and
    /// non-media blobs are refused.
    pub fn media_response(&self, path: &str) -> ApiResult<(String, Vec<u8>)> {
        let Some(token) = path.strip_prefix('/') else {
            return Err(media_not_found());
        };
        if token.is_empty()
            || token.contains('/')
            || token.contains('\\')
            || token.contains('?')
            || token.contains('#')
            || token.contains('%')
            || token.contains("..")
        {
            return Err(media_not_found());
        }
        let mut inner = self.lock();
        let now = self.clock.now();
        inner
            .media_tokens
            .retain(|_, binding| binding.expires_at > now);
        let Some(binding) = inner.media_tokens.get(token).cloned() else {
            return Err(media_not_found());
        };
        let service = inner.service.as_ref().ok_or_else(media_not_found)?;
        let fresh = service.refresh(now, false).map_err(|_| media_not_found())?;
        if !agendar_core::reduce::evidence::blob_referenced(
            &fresh.events,
            &binding.project,
            &binding.blob,
            false,
        ) {
            return Err(media_not_found());
        }
        let bytes = read_blob(service.vault(), &binding.project, &binding.blob)
            .map_err(|_| media_not_found())?;
        if bytes.len() as u64 != binding.blob.bytes {
            return Err(media_not_found());
        }
        Ok((binding.mime, bytes))
    }

    // -----------------------------------------------------------------
    // Notification dispatch
    // -----------------------------------------------------------------

    fn reconcile_notifications(&self, inner: &mut Inner, state: &State, now: UtcTime) {
        // Progress marks advance even while notifications are off, so turning
        // them back on never replays what happened meanwhile.
        // The menu bar glyph follows the same visible count: a snooze that
        // runs out turns it amber again on the next tick.
        self.ui.attention(
            needs_count(state, &inner.preferences, now),
            working_count(state),
        );
        let progress = self.progress_notices(inner, state, now);
        self.withdraw_settled_notices(inner, state, &progress);
        if !inner.preferences.notifications {
            return;
        }
        let mut to_show: Vec<(NoticeSpec, Option<NoticeBinding>)> = progress;
        for (slug, project) in &state.projects {
            for need in &project.needs_you {
                if !notifications::notice_eligible(need) {
                    continue;
                }
                if inner
                    .preferences
                    .snoozed
                    .get(&need.id)
                    .is_some_and(|until| *until > now)
                    || inner.preferences.dismissed.contains(&need.id)
                {
                    continue;
                }
                let decision = project.decisions.get(&need.source);
                let revision = projection::need_revision(need, decision, &project.policy.revision);
                let key = format!("{}:{}", need.id, revision);
                if inner.notice_dedupe.contains(&key) {
                    continue;
                }
                let id = new_notice_id();
                let title = project.summary.title.clone();
                let lang = crate::preferences::Lang::resolve(&inner.preferences.language);
                let spec = notifications::notice_for(id.clone(), &title, need, decision, lang);
                let choices: Vec<NoticeChoice> = decision
                    .map(|decision| {
                        notifications::choice_options(need, Some(decision))
                            .into_iter()
                            .map(|(option_id, label)| {
                                let token = new_token();
                                let request_id = new_request_id();
                                inner.tokens.insert(
                                    token.clone(),
                                    PreparedToken {
                                        surface: "notification".to_owned(),
                                        ask: decision.id.clone(),
                                        option: option_id,
                                        revision: projection::decision_revision(
                                            slug,
                                            decision,
                                            &project.policy.revision,
                                        ),
                                        receipt: decision.effective_receipt.clone(),
                                        label: label.clone(),
                                        risk: Reversibility::Easy,
                                        // Easy risk, but a low-confidence card is
                                        // still classified Choose: keep the class.
                                        guard: guard::guard_for(&decision.card),
                                        // A button named after its option is a
                                        // deliberate choice, not a one-key default.
                                        explicit_choice: true,
                                        expires_at: now.add_millis(guard::TOKEN_TTL_S * 1_000),
                                        used: false,
                                        notice_id: Some(id.clone()),
                                        request_id: Some(request_id.clone()),
                                        own_answer: None,
                                    },
                                );
                                NoticeChoice {
                                    token,
                                    request_id,
                                    label,
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let binding = Some(NoticeBinding {
                    item_id: need.id.clone(),
                    project: slug.clone(),
                    choices,
                    answered: false,
                    withdrawn: false,
                });
                to_show.push((spec, binding));
                inner.notice_dedupe.insert(key);
            }
        }
        for (spec, binding) in to_show {
            match self.sink.show(&spec) {
                Ok(()) => {
                    if let Some(binding) = binding {
                        inner.notices.insert(spec.id.clone(), binding);
                    } else {
                        inner.notices.insert(
                            spec.id.clone(),
                            NoticeBinding {
                                item_id: spec.item_id.clone(),
                                project: ProjectSlug::parse(&spec.project).unwrap_or_else(|_| {
                                    ProjectSlug::parse("unknown").expect("slug")
                                }),
                                choices: Vec::new(),
                                answered: false,
                                withdrawn: false,
                            },
                        );
                    }
                }
                Err(_) => {
                    // Not displayed: drop the dedupe marker so a later tick can
                    // retry once the capability recovers.
                    inner
                        .notice_dedupe
                        .retain(|key| !key.starts_with(&format!("{}:", spec.item_id)));
                }
            }
        }
    }

    /// Take down notices that no longer ask anything: a question that was
    /// answered or closed, and a project's progress notice a newer one replaces.
    /// The platform keeps only a few live notices, so stale ones must go.
    fn withdraw_settled_notices(
        &self,
        inner: &mut Inner,
        state: &State,
        replacing: &[(NoticeSpec, Option<NoticeBinding>)],
    ) {
        let current: BTreeSet<&str> = state
            .projects
            .values()
            .flat_map(|project| project.needs_you.iter().map(|need| need.id.as_str()))
            .collect();
        let replaced: BTreeSet<&str> = replacing
            .iter()
            .map(|(spec, _)| spec.item_id.as_str())
            .collect();
        let stale: Vec<String> = inner
            .notices
            .iter()
            .filter(|(_, binding)| !binding.withdrawn)
            .filter(|(_, binding)| {
                if binding.item_id.starts_with("project:") {
                    replaced.contains(binding.item_id.as_str())
                } else {
                    !current.contains(binding.item_id.as_str())
                }
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            let _ = self.sink.withdraw(&id);
            if let Some(binding) = inner.notices.get_mut(&id) {
                binding.withdrawn = true;
            }
        }
    }

    /// Milestone-finished and work-stopped notices (Open only), at most one per
    /// project per tick. The first sight of a project only seeds its mark.
    fn progress_notices(
        &self,
        inner: &mut Inner,
        state: &State,
        now: UtcTime,
    ) -> Vec<(NoticeSpec, Option<NoticeBinding>)> {
        let lang = crate::preferences::Lang::resolve(&inner.preferences.language);
        let marks = inner.progress_marks.get_or_insert_with(BTreeMap::new);
        marks.retain(|slug, _| state.projects.contains_key(slug));
        let mut notices = Vec::new();
        for (slug, project) in &state.projects {
            if project.summary.archived {
                continue;
            }
            let facts = notifications::ProgressFacts::of(project);
            let Some(mark) = marks.get_mut(slug) else {
                marks.insert(slug.clone(), notifications::ProgressMark::seed(&facts));
                continue;
            };
            if let Some((title, body)) =
                notifications::progress_notice(&project.summary.title, mark, &facts, now, lang)
            {
                notices.push((
                    NoticeSpec {
                        id: new_notice_id(),
                        title,
                        body,
                        choices: Vec::new(),
                        project: slug.as_str().to_owned(),
                        item_id: format!("project:{slug}"),
                    },
                    None,
                ));
            }
        }
        notices
    }

    /// Record a human handoff/pause intent for one current rescue row. It only
    /// writes a plan patch; no process is started, resumed or stopped.
    pub fn resolve_rescue(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?.to_owned();
        EventId::parse(&request_id).map_err(AppError::from)?;
        let item_id = required_str(request, "item_id")?.to_owned();
        let action = match required_str(request, "action")? {
            "handoff" => RescueAction::Handoff,
            "pause" => RescueAction::Pause,
            other => {
                return Err(AppError::validation(
                    "/action",
                    format!("Unknown rescue action `{other}`; use handoff or pause."),
                ));
            }
        };
        let expected = request
            .get("expected_rescue_revision")
            .and_then(Value::as_str)
            .map(Sha256::parse)
            .transpose()
            .map_err(AppError::from)?;
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(&request_id) {
            if entry.command == "resolve_rescue" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        let data = {
            let service = inner
                .service
                .as_ref()
                .ok_or_else(|| not_configured(&inner))?;
            let now = self.clock.now();
            let fresh = service.refresh(now, false).map_err(AppError::from)?;
            let mut found = None;
            for (slug, project) in &fresh.state.projects {
                if let Some(need) = project.needs_you.iter().find(|need| need.id == item_id) {
                    found = Some((
                        slug.clone(),
                        need.clone(),
                        project.policy.revision.clone(),
                        project.decisions.get(&need.source),
                    ));
                    break;
                }
            }
            let Some((slug, need, policy, decision)) = found else {
                return Err(AppError::new(
                    ErrorCode::StaleAnswer.as_str(),
                    "This rescue is no longer current.",
                )
                .with_hint("Refresh the board before acting."));
            };
            if let Some(expected) = expected
                && projection::need_revision(&need, decision, &policy) != expected
            {
                return Err(stale_answer());
            }
            let resolved = service
                .resolve_rescue(&slug, &item_id, action)
                .map_err(AppError::from)?;
            json!({"event_id": resolved["event_id"].clone()})
        };
        inner.requests.insert(
            request_id,
            RequestEntry {
                command: "resolve_rescue",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    pub fn notification_capability(&self) -> (Availability, Option<String>) {
        let inner = self.lock();
        if !inner.preferences.notifications {
            return (
                Availability::Unknown,
                Some("notification consent has not been granted".to_owned()),
            );
        }
        (self.sink.capability(), self.sink.reason())
    }

    pub fn handle_notice(
        &self,
        notice_id: &str,
        action: NoticeAction,
        origin: Origin,
    ) -> NoticeOutcome {
        let binding = {
            let inner = self.lock();
            inner.notices.get(notice_id).cloned()
        };
        let Some(binding) = binding else {
            return NoticeOutcome::Unknown;
        };
        let chosen = match action {
            NoticeAction::Open => None,
            NoticeAction::Choose(index) => binding.choices.get(index).cloned(),
        };
        let Some(choice) = chosen.filter(|_| origin == Origin::Live) else {
            let status = if action == NoticeAction::Open {
                "open"
            } else {
                "review_current"
            };
            return self.route(&binding, status);
        };
        if binding.answered {
            return NoticeOutcome::Answered { duplicate: true };
        }
        let mut inner = self.lock();
        // Any button of this notice that already answered makes the rest a
        // duplicate: one question, one answer.
        if binding
            .choices
            .iter()
            .any(|known| inner.journal.contains_key(&known.request_id))
        {
            drop(inner);
            return NoticeOutcome::Answered { duplicate: true };
        }
        let digest =
            guard::payload_digest(&json!(["notification", notice_id, "choose", choice.label]));
        match self.commit_answer(
            &mut inner,
            "notification",
            &choice.token,
            &choice.label,
            None,
            None,
        ) {
            Ok(data) => {
                inner.journal.insert(
                    choice.request_id,
                    JournalEntry {
                        command: "notification_choose".to_owned(),
                        digest,
                        data,
                    },
                );
                NoticeOutcome::Answered { duplicate: false }
            }
            Err(error) => {
                drop(inner);
                if error.code.as_str() == ErrorCode::StaleAnswer.as_str()
                    || error.code.as_str() == ErrorCode::Validation.as_str()
                    || error.code.as_str() == ErrorCode::Withdrawn.as_str()
                {
                    return self.route(&binding, "review_current");
                }
                NoticeOutcome::Routed {
                    item_id: binding.item_id.clone(),
                    status: "review_current".to_owned(),
                }
            }
        }
    }

    fn route(&self, binding: &NoticeBinding, status: &str) -> NoticeOutcome {
        let focus = {
            let mut inner = self.lock();
            let vault_id = inner
                .service
                .as_ref()
                .map(|service| service.vault().id().as_str().to_owned())
                .unwrap_or_default();
            let sequence = inner.navigation.next_sequence.saturating_add(1);
            inner.navigation.next_sequence = sequence;
            let focus = json!({
                "navigation_epoch": inner.navigation.epoch,
                "sequence": sequence,
                "vault_id": vault_id,
                "project": binding.project.as_str(),
                "item_id": binding.item_id,
                "status": status,
            });
            inner.navigation.focus = Some(focus.clone());
            focus
        };
        self.ui.focus(&focus);
        let sequence = focus
            .get("sequence")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        match status {
            "open" => NoticeOutcome::Focused {
                item_id: binding.item_id.clone(),
                sequence,
            },
            _ => NoticeOutcome::Routed {
                item_id: binding.item_id.clone(),
                status: status.to_owned(),
            },
        }
    }

    // -----------------------------------------------------------------
    // Local configuration and installation
    // -----------------------------------------------------------------

    pub fn show_main(&self) {
        self.ui.show_main();
    }

    /// Bring the main window forward on one Needs-you item (from the tray):
    /// the same focus event a notification click sends, then the window.
    pub fn show_main_on(&self, project: &ProjectSlug, item_id: &str) {
        let binding = NoticeBinding {
            item_id: item_id.to_owned(),
            project: project.clone(),
            choices: Vec::new(),
            answered: false,
            withdrawn: false,
        };
        let _ = self.route(&binding, "open");
        self.ui.show_main();
    }

    pub fn hide_popover(&self) {
        self.ui.hide_popover();
    }

    pub fn set_preferences(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?;
        EventId::parse(request_id).map_err(AppError::from)?;
        let expected =
            Sha256::parse(required_str(request, "expected_revision")?).map_err(AppError::from)?;
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(request_id) {
            if entry.command == "set_preferences" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        if inner.preferences.wire_revision() != expected {
            return Err(stale_answer());
        }
        let mut next = inner.preferences.clone();
        if let Some(theme) = optional_str(request, "theme") {
            if !matches!(theme, "system" | "light" | "dark") {
                return Err(AppError::validation(
                    "/theme",
                    "theme must be system, light or dark.",
                ));
            }
            next.theme = theme.to_owned();
        }
        if let Some(language) = optional_str(request, "language") {
            if !crate::preferences::LANGUAGES.contains(&language) {
                return Err(AppError::validation(
                    "/language",
                    "language must be auto, en, zh-Hans or zh-Hant.",
                ));
            }
            next.language = language.to_owned();
        }
        if let Some(motion) = request.get("motion").and_then(Value::as_bool) {
            next.motion = motion;
        }
        if let Some(notifications) = request.get("notifications").and_then(Value::as_bool) {
            next.notifications = notifications;
        }
        if let Some(snooze) = request.get("snooze")
            && let Value::Object(object) = snooze
        {
            let item_id = object
                .get("item_id")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::validation("/snooze/item_id", "item_id is required."))?;
            let until_text = object
                .get("until")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::validation("/snooze/until", "until is required."))?;
            let until = UtcTime::parse(until_text).map_err(AppError::from)?;
            let now = self.clock.now();
            if until.millis() > now.add_millis(7 * 24 * 60 * 60 * 1_000).millis() {
                return Err(AppError::validation(
                    "/snooze/until",
                    "A snooze can last at most seven days.",
                ));
            }
            next.snoozed.insert(item_id.to_owned(), until);
        }
        // An ended snooze is gone: a restore sends a time already passed, and
        // old entries must not pile up in the local preferences.
        let now = self.clock.now();
        next.snoozed.retain(|_, until| *until > now);
        if request.get("mark_looked").and_then(Value::as_bool) == Some(true) {
            next.last_looked_at = Some(self.clock.now());
            if let Some(service) = inner.service.as_ref()
                && let Ok(fresh) = service.refresh(self.clock.now(), true)
            {
                next.last_looked_ids = fresh
                    .state
                    .projects
                    .values()
                    .flat_map(|project| project.needs_you.iter())
                    .map(|need| need.source.as_str().to_owned())
                    .take(128)
                    .collect();
            }
        }
        next.revision = next.revision.saturating_add(1);
        next.write(&self.data_root).map_err(AppError::from)?;
        inner.preferences = next.clone();
        crate::preferences::set_active_lang(crate::preferences::Lang::resolve(&next.language));
        let data = json!({"preferences": next.view()});
        if next.notifications {
            self.ui.start_notifications();
        }
        inner.requests.insert(
            request_id.to_owned(),
            RequestEntry {
                command: "set_preferences",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    /// Recount what needs the person after a local snooze or dismissal, so
    /// the menu bar glyph changes at once instead of at the next tick.
    fn refresh_attention(&self, inner: &Inner, now: UtcTime) {
        if let Some(service) = inner.service.as_ref()
            && let Ok(fresh) = service.refresh(now, true)
        {
            self.ui.attention(
                needs_count(&fresh.state, &inner.preferences, now),
                working_count(&fresh.state),
            );
        }
    }

    /// Snooze one Needs-you item, or end its snooze with a time already
    /// passed. Local only, and it patches that one entry, so it needs no
    /// whole-preferences revision (a mark-looked write in between never
    /// refuses it) and the tray may call it. Only an item that is waiting now
    /// can be snoozed, for at most seven days.
    pub fn snooze_need(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?;
        EventId::parse(request_id).map_err(AppError::from)?;
        let item_id = required_str(request, "item_id")?.to_owned();
        let until = UtcTime::parse(required_str(request, "until")?).map_err(AppError::from)?;
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(request_id) {
            if entry.command == "snooze_need" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        let now = self.clock.now();
        if until.millis() > now.add_millis(7 * 24 * 60 * 60 * 1_000).millis() {
            return Err(AppError::validation(
                "/until",
                "A snooze can last at most seven days.",
            ));
        }
        if until > now {
            let waiting = inner
                .service
                .as_ref()
                .and_then(|service| service.refresh(now, true).ok())
                .is_some_and(|fresh| {
                    fresh
                        .state
                        .projects
                        .values()
                        .any(|project| project.needs_you.iter().any(|need| need.id == item_id))
                });
            if !waiting {
                return Err(AppError::new(
                    ErrorCode::NotFound.as_str(),
                    "This item is no longer waiting for you.",
                ));
            }
        }
        let mut next = inner.preferences.clone();
        next.snoozed.insert(item_id, until);
        next.snoozed.retain(|_, until| *until > now);
        next.revision = next.revision.saturating_add(1);
        next.write(&self.data_root).map_err(AppError::from)?;
        inner.preferences = next.clone();
        self.refresh_attention(&inner, now);
        let data = json!({"preferences": next.view()});
        inner.requests.insert(
            request_id.to_owned(),
            RequestEntry {
                command: "snooze_need",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    /// Close one hand-off item for good on this computer (the user's Cancel).
    /// The step is untouched: it stays stopped on its project page, where the
    /// hand-off can still be chosen. Only a hand-off item waiting now can be
    /// closed; the remembered set keeps only items still waiting.
    pub fn dismiss_need(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?;
        EventId::parse(request_id).map_err(AppError::from)?;
        let item_id = required_str(request, "item_id")?.to_owned();
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(request_id) {
            if entry.command == "dismiss_need" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        let now = self.clock.now();
        let waiting: BTreeSet<String> = inner
            .service
            .as_ref()
            .and_then(|service| service.refresh(now, true).ok())
            .map(|fresh| {
                fresh
                    .state
                    .projects
                    .values()
                    .flat_map(|project| project.needs_you.iter())
                    .filter(|need| need.kind == NeedsKind::Rescue)
                    .map(|need| need.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        if !waiting.contains(&item_id) {
            return Err(AppError::new(
                ErrorCode::NotFound.as_str(),
                "Only a hand-off item that is waiting now can be closed.",
            ));
        }
        let mut next = inner.preferences.clone();
        next.dismissed.retain(|id| waiting.contains(id));
        next.dismissed.insert(item_id);
        next.revision = next.revision.saturating_add(1);
        next.write(&self.data_root).map_err(AppError::from)?;
        inner.preferences = next.clone();
        self.refresh_attention(&inner, now);
        let data = json!({"preferences": next.view()});
        inner.requests.insert(
            request_id.to_owned(),
            RequestEntry {
                command: "dismiss_need",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    /// Link a repository to a project in this host's local config. This is a
    /// local convenience for the setup/project view; it writes no Vault event.
    pub fn link_repo(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?.to_owned();
        EventId::parse(&request_id).map_err(AppError::from)?;
        let repo = required_str(request, "repo")?.trim().to_owned();
        let project =
            ProjectSlug::parse(required_str(request, "project")?).map_err(AppError::from)?;
        if repo.is_empty() {
            return Err(AppError::validation("/repo", "Choose a repository path."));
        }
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(&request_id) {
            if entry.command == "link_repo" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        // The request states the whole link the user asked for, so it needs no
        // guard on the rest of the config, which agents rewrite on every start.
        Config::map_repo(&self.data_root, Path::new(&repo), &project).map_err(AppError::from)?;
        let config = Config::load_or_default(&self.data_root).map_err(AppError::from)?;
        let canonical = Path::new(&repo)
            .canonicalize()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| repo.clone());
        let data = json!({
            "repo": canonical,
            "project": project,
            "expected_config_revision": config_revision(&config),
        });
        inner.config = config;
        inner.requests.insert(
            request_id,
            RequestEntry {
                command: "link_repo",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    /// Remove a repository link from this computer's configuration. Only the
    /// local mapping changes; no Vault event is written.
    pub fn unlink_repo(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?.to_owned();
        EventId::parse(&request_id).map_err(AppError::from)?;
        let repo = required_str(request, "repo")?.to_owned();
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(&request_id) {
            if entry.command == "unlink_repo" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        // Guard only the link being removed: the rest of the config changes
        // whenever an agent starts, and that must not refuse the click.
        let expected = optional_str(request, "expected_project")
            .map(ProjectSlug::parse)
            .transpose()
            .map_err(AppError::from)?;
        let unlinked = Config::update(&self.data_root, |config| match config.repos.get(&repo) {
            Some(current)
                if expected
                    .as_ref()
                    .is_some_and(|expected| expected != current) =>
            {
                Err(stale_config_error())
            }
            _ => Ok(config.repos.remove(&repo)),
        })
        .map_err(AppError::from)?;
        let config = Config::load_or_default(&self.data_root).map_err(AppError::from)?;
        let project = unlinked.clone().or(expected);
        let project_still_linked = project
            .as_ref()
            .is_some_and(|project| config.repos.values().any(|linked| linked == project));
        let data = json!({
            "repo": repo,
            "removed": unlinked.is_some(),
            "project": project,
            "project_still_linked": project_still_linked,
            "expected_config_revision": config_revision(&config),
        });
        inner.config = config;
        inner.requests.insert(
            request_id,
            RequestEntry {
                command: "unlink_repo",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    /// Read-only local configuration for the Settings page. Never writes.
    pub fn get_local_config(&self) -> ApiResult<Value> {
        let config = Config::load_or_default(&self.data_root).map_err(AppError::from)?;
        Ok(json!({
            "vault": config.vault,
            "data_dir": self.data_root.to_string_lossy(),
            "config_revision": config_revision(&config),
            "repos": config.repos,
            "codex_bin": config.codex_bin,
            "cli_path": bundled_cli_path().ok().map(|path| path.to_string_lossy().into_owned()),
            "app_version": env!("CARGO_PKG_VERSION"),
        }))
    }

    /// Read-only suggested Vault locations for first-run Setup.
    pub fn get_setup_locations(&self) -> ApiResult<Value> {
        let locations = crate::setup_locations::current().ok_or_else(|| {
            AppError::new(
                ErrorCode::Context.as_str(),
                "Cannot find this computer's home folder for Setup suggestions.",
            )
        })?;
        Ok(json!({ "locations": locations }))
    }

    pub fn configure_vault(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?;
        EventId::parse(request_id).map_err(AppError::from)?;
        let consent = request.get("consent").and_then(Value::as_bool) == Some(true);
        if !consent {
            return Err(AppError::new(
                ErrorCode::Consent.as_str(),
                "Explicit consent is required before opening or creating a Vault.",
            )
            .with_hint("Tick the confirmation box and retry."));
        }
        let path = required_str(request, "path")?.trim().to_owned();
        if path.is_empty() {
            return Err(AppError::validation("/path", "Choose a Vault folder."));
        }
        let create = request.get("create").and_then(Value::as_bool) == Some(true);
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(request_id) {
            if entry.command == "configure_vault" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        let expected = Sha256::parse(required_str(request, "expected_config_revision")?)
            .map_err(AppError::from)?;
        self.reload_config(&mut inner);
        if config_revision(&inner.config) != expected {
            return Err(AppError::from(stale_config_error()));
        }
        let path_buf = PathBuf::from(&path);
        if create {
            std::fs::create_dir_all(&path_buf).map_err(|error| {
                AppError::io(format!("Cannot create the Vault folder: {error}"))
            })?;
        } else if !path_buf.is_dir() {
            return Err(AppError::validation(
                "/path",
                "The Vault folder does not exist; enable Create to create it.",
            ));
        }
        let path_buf = path_buf.canonicalize().map_err(|error| {
            AppError::validation("/path", format!("Cannot resolve the Vault folder: {error}"))
        })?;
        let (mut service, _created) =
            Service::init(&path_buf, &self.data_root).map_err(AppError::from)?;
        apply_bundled_cli(&mut service);
        Config::select(&self.data_root, Some(&path_buf), None).map_err(AppError::from)?;
        let config = Config::load_or_default(&self.data_root).map_err(AppError::from)?;
        let data = json!({
            "vault_id": service.vault().id().as_str(),
            "host": service.host().as_str(),
        });
        let vault_id = service.vault().id().as_str().to_owned();
        let host = service.host().as_str().to_owned();
        inner.service = Some(std::sync::Arc::new(service));
        inner.config = config;
        inner.open_error = None;
        inner.notice_dedupe.clear();
        inner.notices.clear();
        inner.progress_marks = None;
        inner.tokens.clear();
        inner.media_tokens.clear();
        inner.navigation.focus = None;
        // A different Vault is a different history: re-seed publication state.
        inner.seen_events.clear();
        inner.previous_state = None;
        inner.unblocked.clear();
        inner.releases.clear();
        inner.hydrated = false;
        inner.requests.insert(
            request_id.to_owned(),
            RequestEntry {
                command: "configure_vault",
                digest,
                data: data.clone(),
            },
        );
        let consented = inner.preferences.notifications;
        drop(inner);
        if consented {
            self.ui.start_notifications();
        }
        Ok(json!({"vault_id": vault_id, "host": host}))
    }

    pub fn install_local(&self, request: &Value) -> ApiResult<Value> {
        let request_id = required_str(request, "request_id")?;
        EventId::parse(request_id).map_err(AppError::from)?;
        let kind = required_str(request, "kind")?;
        let dry_run = request.get("dry_run").and_then(Value::as_bool) == Some(true);
        let consent = request.get("consent").and_then(Value::as_bool) == Some(true);
        let expected = optional_str(request, "expected_plan_digest")
            .map(Sha256::parse)
            .transpose()
            .map_err(AppError::from)?;
        let destination = optional_str(request, "destination").map(PathBuf::from);
        let harness = optional_str(request, "harness");
        let mut digest_value = request.clone();
        if let Value::Object(object) = &mut digest_value {
            object.remove("request_id");
        }
        let digest = guard::payload_digest(&digest_value);
        let mut inner = self.lock();
        if let Some(entry) = inner.requests.get(request_id) {
            if entry.command == "install_local" && entry.digest == digest {
                return Ok(entry.data.clone());
            }
            return Err(request_conflict());
        }
        let home = home_dir().map_err(AppError::from)?;
        let (plan, installed) = match kind {
            "path_link" => {
                let cli = bundled_cli_path()?;
                let plan =
                    plan_path_link(&cli, destination.as_deref(), &home).map_err(AppError::from)?;
                if dry_run {
                    (plan, false)
                } else {
                    if !consent {
                        return Err(AppError::new(
                            ErrorCode::Consent.as_str(),
                            "Explicit consent is required before writing the PATH link.",
                        ));
                    }
                    let expected = expected.clone().ok_or_else(|| {
                        AppError::validation(
                            "/expected_plan_digest",
                            "A non-dry-run install must echo the plan digest.",
                        )
                    })?;
                    let done = install_path_link(&cli, destination.as_deref(), &home, &expected)
                        .map_err(AppError::from)?;
                    (done, true)
                }
            }
            "hooks" => {
                let cli = bundled_cli_path()?;
                if harness != Some("claude_code") {
                    return Err(AppError::new(
                        ErrorCode::Unsupported.as_str(),
                        "This harness has no supported hook installer yet.",
                    )
                    .with_hint("Only claude_code is implemented."));
                }
                let plan = plan_claude_hooks(&cli, destination.as_deref(), &home)
                    .map_err(AppError::from)?;
                if dry_run {
                    (plan, false)
                } else {
                    if !consent {
                        return Err(AppError::new(
                            ErrorCode::Consent.as_str(),
                            "Explicit consent is required before writing the hook settings.",
                        ));
                    }
                    let expected = expected.ok_or_else(|| {
                        AppError::validation(
                            "/expected_plan_digest",
                            "A non-dry-run install must echo the plan digest.",
                        )
                    })?;
                    let done = install_claude_hooks(&cli, destination.as_deref(), &home, &expected)
                        .map_err(AppError::from)?;
                    (done, true)
                }
            }
            "observe_hooks" => {
                let cli = bundled_cli_path()?;
                if harness != Some("claude_code") {
                    return Err(AppError::new(
                        ErrorCode::Unsupported.as_str(),
                        "This harness has no supported hook installer yet.",
                    )
                    .with_hint("Only claude_code is implemented."));
                }
                let plan =
                    plan_claude_hook_set(HookSet::Observe, &cli, destination.as_deref(), &home)
                        .map_err(AppError::from)?;
                if dry_run {
                    (plan, false)
                } else {
                    if !consent {
                        return Err(AppError::new(
                            ErrorCode::Consent.as_str(),
                            "Explicit consent is required before writing the hook settings.",
                        ));
                    }
                    let expected = expected.ok_or_else(|| {
                        AppError::validation(
                            "/expected_plan_digest",
                            "A non-dry-run install must echo the plan digest.",
                        )
                    })?;
                    let done = install_claude_hook_set(
                        HookSet::Observe,
                        &cli,
                        destination.as_deref(),
                        &home,
                        &expected,
                    )
                    .map_err(AppError::from)?;
                    (done, true)
                }
            }
            other => {
                return Err(AppError::validation(
                    "/kind",
                    format!("Unknown install kind {other}."),
                ));
            }
        };
        let data = json!({"plan": plan, "installed": installed});
        inner.requests.insert(
            request_id.to_owned(),
            RequestEntry {
                command: "install_local",
                digest,
                data: data.clone(),
            },
        );
        Ok(data)
    }

    // -----------------------------------------------------------------
    // Watcher and steward hooks
    // -----------------------------------------------------------------

    pub fn current_vault_root(&self) -> Option<PathBuf> {
        let inner = self.lock();
        inner
            .service
            .as_ref()
            .map(|service| service.vault().root().to_path_buf())
    }

    pub fn start_watch(&self, root: &Path) -> Option<VaultWatch> {
        let inner = self.lock();
        let service = inner.service.as_ref()?;
        if service.vault().root() != root {
            return None;
        }
        VaultWatch::start(service.vault()).ok()
    }

    pub fn watch_tick(&self, batch: &str) {
        if self.is_shutdown() {
            return;
        }
        if batch == "time" {
            self.refresh_usage();
        }
        let mut inner = self.lock();
        let now = self.clock.now();
        let Ok((state, events, generation)) = self.fresh_state_with_events(&inner, now) else {
            return;
        };
        if !inner.hydrated {
            // A tick may arrive before the first read: seed history instead of
            // animating everything that already existed.
            self.hydrate(&mut inner, &state, &events);
            inner.hydrated = true;
            return;
        }
        self.publish(&mut inner, &state, &events, generation, now, batch);
        if batch == "time" {
            // Permission can change in System Settings while Agendar runs.
            self.sink.refresh();
        }
        self.reconcile_notifications(&mut inner, &state, now);
    }

    pub fn steward_tick(&self) {
        if self.is_shutdown() {
            return;
        }
        // Clone the shared service and release the UI lock before any transport:
        // a Codex queue call may take its full bounded budget and must never
        // block a read or a human action.
        let service = self.lock().service.clone();
        if let Some(service) = service {
            // A lost session or a queued delivery is a state change; the
            // watcher's next change event publishes it. No fake progress is
            // written.
            let _ = agendar_core::runtime::steward::steward_tick(&service);
        }
    }

    /// Test/inspection helper: the complete operator events in the Vault.
    pub fn event_lines(&self) -> Vec<String> {
        let inner = self.lock();
        let Some(service) = inner.service.as_ref() else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        collect_lines(service.vault(), &mut lines);
        lines
    }

    /// Token usage per project from the last log scan (scans once if never).
    fn tokens_lookup(
        &self,
        config: &Config,
        now: UtcTime,
    ) -> impl Fn(&ProjectSlug) -> Option<agendar_core::runtime::usage::TokenUsage> + use<> {
        let since = now.millis() - agendar_core::runtime::usage::WINDOW_MS;
        let config = config.clone();
        let usage = self.usage.lock().ok().map(|scanner| {
            config
                .repos
                .values()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|slug| {
                    (
                        slug.clone(),
                        scanner.usage_for(&project_roots(&config, slug), since),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        });
        move |slug: &ProjectSlug| {
            usage
                .as_ref()
                .and_then(|map| map.get(slug).copied().flatten())
        }
    }

    /// Agents seen through the observe-only hooks, minus the ones that already
    /// report through the skill (same harness process). Each names the linked
    /// project its folder belongs to, when one does. Never a question.
    fn presence_rows(&self, inner: &Inner, now: UtcTime) -> Value {
        use agendar_core::runtime::presence;
        let file = presence::load(&self.data_root);
        let skill_pids: BTreeSet<u32> = inner
            .service
            .as_ref()
            .and_then(|service| service.runtime().ok())
            .map(|runtime| {
                runtime
                    .sessions
                    .values()
                    .filter(|session| !session.lost && !session.sealed)
                    .map(|session| session.pid)
                    .collect()
            })
            .unwrap_or_default();
        let rows: Vec<Value> = file
            .agents
            .iter()
            .filter(|(_, agent)| {
                now.millis() - agent.updated_at.millis() <= presence::PRESENCE_TTL_MS
            })
            .filter(|(_, agent)| agent.pid.is_none_or(|pid| !skill_pids.contains(&pid)))
            .map(|(key, agent)| {
                // A folder inside a linked repository, or one level above it
                // (never the filesystem root or a home folder).
                // Linked repositories are stored canonical (macOS /var is
                // /private/var), so compare the agent's folder the same way.
                let cwd = std::fs::canonicalize(&agent.cwd).unwrap_or_else(|_| agent.cwd.clone());
                let deep_enough = cwd.components().count() > 3;
                let project = inner
                    .config
                    .repos
                    .iter()
                    .find(|(path, _)| {
                        let root = Path::new(path.as_str());
                        cwd.starts_with(root) || (deep_enough && root.starts_with(&cwd))
                    })
                    .map(|(_, slug)| slug.clone());
                json!({
                    "key": key,
                    "harness": agent.harness,
                    "state": agent.state,
                    "since": agent.since,
                    "folder": agent
                        .cwd
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    "project": project,
                })
            })
            .collect();
        json!(rows)
    }

    /// Re-read the agents' local logs for token counts (outside the main lock;
    /// only new bytes are read).
    pub fn refresh_usage(&self) {
        let Ok(home) = agendar_core::runtime::install::home_dir() else {
            return;
        };
        let now = self.clock.now().millis();
        if let Ok(mut usage) = self.usage.lock() {
            usage.refresh(&home, now);
        }
    }

    /// Open a report document in the person's own browser (or default viewer).
    /// Only a blob a report, card or delivery in that project references, and
    /// only after its digest is verified.
    pub fn open_report(&self, request: &Value) -> ApiResult<Value> {
        let project =
            ProjectSlug::parse(required_str(request, "project")?).map_err(AppError::from)?;
        let blob: BlobRef = serde_json::from_value(
            request
                .get("blob")
                .cloned()
                .ok_or_else(|| AppError::validation("/blob", "blob is required."))?,
        )
        .map_err(|error| AppError::validation("/blob", format!("Invalid blob: {error}.")))?;
        let inner = self.lock();
        let service = inner
            .service
            .as_ref()
            .ok_or_else(|| not_configured(&inner))?
            .clone();
        drop(inner);
        let fresh = service
            .refresh(self.clock.now(), false)
            .map_err(AppError::from)?;
        if !agendar_core::reduce::evidence::blob_referenced(&fresh.events, &project, &blob, true) {
            return Err(AppError::new(
                ErrorCode::NotFound.as_str(),
                "That report is not part of this project.",
            ));
        }
        let path = agendar_core::vault::blobs::verified_blob_path(service.vault(), &project, &blob)
            .map_err(AppError::from)?;
        open_with_system(&path).map_err(|error| {
            AppError::new(
                ErrorCode::Io.as_str(),
                format!("Cannot open the report: {error}."),
            )
        })?;
        Ok(json!({"opened": true}))
    }

    /// The local Motion preference (the menu bar glyph animates only with it).
    pub fn motion(&self) -> bool {
        self.lock().preferences.motion
    }

    /// Test/inspection helper: the current number of notices displayed.
    pub fn live_notice_count(&self) -> usize {
        self.lock().notices.len()
    }
}

const MEDIA_TOKEN_TTL_S: i64 = 60;

fn find_delivery<'a>(
    state: &'a State,
    id: &EventId,
) -> Option<(
    &'a ProjectSlug,
    &'a agendar_core::model::ProjectState,
    &'a agendar_core::model::DeliveryState,
)> {
    for (slug, project) in &state.projects {
        if let Some(delivery) = project.deliveries.get(id) {
            return Some((slug, project, delivery));
        }
    }
    None
}

fn current_receipt<'a>(
    project: &'a agendar_core::model::ProjectState,
    delivery: &agendar_core::model::DeliveryState,
) -> Option<&'a EventId> {
    project
        .receipts
        .get(&delivery.receipt)
        .and_then(|receipt| project.decisions.get(&receipt.ask))
        .and_then(|decision| decision.effective_receipt.as_ref())
}

fn mime_for(ext: BlobExt) -> Option<&'static str> {
    match ext {
        BlobExt::Txt => Some("text/plain"),
        BlobExt::Md => Some("text/markdown"),
        BlobExt::Html => Some("text/html"),
        BlobExt::Png => Some("image/png"),
        BlobExt::Jpg => Some("image/jpeg"),
        BlobExt::Webp => Some("image/webp"),
        BlobExt::Mp4 => Some("video/mp4"),
        BlobExt::Webm => Some("video/webm"),
        BlobExt::Svg => Some("image/svg+xml"),
    }
}

fn is_text(ext: BlobExt) -> bool {
    matches!(ext, BlobExt::Txt | BlobExt::Md | BlobExt::Html)
}

fn is_media(ext: BlobExt) -> bool {
    matches!(
        ext,
        BlobExt::Png | BlobExt::Jpg | BlobExt::Webp | BlobExt::Mp4 | BlobExt::Webm
    )
}

fn check_result_str(result: CheckResult) -> &'static str {
    match result {
        CheckResult::Passed => "passed",
        CheckResult::Failed => "failed",
        CheckResult::NotProven => "not_proven",
    }
}

fn media_not_found() -> AppError {
    AppError::new(ErrorCode::NotFound.as_str(), "Media is not available.")
        .with_hint("The media link may have expired; reopen the report or delivery.")
}

/// The `expected_fields` map: exactly the patched field names and their SHA256
/// revision tokens from the projection. Callers never send a raw event ID.
fn parse_expected_fields(value: Option<&Value>) -> ApiResult<BTreeMap<String, Sha256>> {
    let Some(Value::Object(map)) = value else {
        return Err(AppError::validation(
            "/expected_fields",
            "expected_fields is required.",
        ));
    };
    let mut fields = BTreeMap::new();
    for (field, token) in map {
        if field.trim().is_empty() || field.chars().count() > 40 {
            return Err(AppError::validation(
                "/expected_fields",
                "expected_fields contains an invalid field name.",
            ));
        }
        let token = token.as_str().ok_or_else(|| {
            AppError::validation(
                "/expected_fields",
                "expected_fields values must be revision strings.",
            )
        })?;
        fields.insert(field.clone(), Sha256::parse(token).map_err(AppError::from)?);
    }
    Ok(fields)
}

/// One standalone house rule as the UI sends it (`RuleSet` plus validation).
fn parse_rule(value: Option<&Value>) -> ApiResult<RuleSet> {
    let value = value.ok_or_else(|| AppError::validation("/rule", "rule is required."))?;
    let rule_id = agendar_core::model::RuleId::parse(
        value
            .get("rule_id")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::validation("/rule/rule_id", "rule_id is required."))?,
    )
    .map_err(AppError::from)?;
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::validation("/rule/text", "text is required."))?
        .trim()
        .to_owned();
    if text.trim().is_empty() || text.trim().chars().count() > 500 {
        return Err(AppError::validation(
            "/rule/text",
            "A house rule must be 1..=500 characters.",
        ));
    }
    let enabled = value
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    Ok(RuleSet {
        rule_id,
        text,
        enabled,
    })
}

/// `save_rule` from `review_delivery` has no `enabled` field: saving a rule
/// always enables it.
fn parse_save_rule(value: Option<&Value>) -> ApiResult<Option<RuleSet>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let rule_id = agendar_core::model::RuleId::parse(
                value
                    .get("rule_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        AppError::validation("/save_rule/rule_id", "rule_id is required.")
                    })?,
            )
            .map_err(AppError::from)?;
            let text = value
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::validation("/save_rule/text", "text is required."))?
                .trim()
                .to_owned();
            if text.trim().is_empty() || text.trim().chars().count() > 500 {
                return Err(AppError::validation(
                    "/save_rule/text",
                    "A house rule must be 1..=500 characters.",
                ));
            }
            Ok(Some(RuleSet {
                rule_id,
                text,
                enabled: true,
            }))
        }
    }
}

fn collect_lines(vault: &Vault, lines: &mut Vec<String>) {
    collect_jsonl(&vault.root().join("projects"), lines);
}

fn collect_jsonl(directory: &std::path::Path, lines: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, lines);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            lines.extend(
                text.lines()
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned),
            );
        }
    }
}

/// Items that need the human across every project (drives the tray glyph).
/// Projects with at least one agent at work now (live session), archived ones
/// excluded: the number beside the menu bar icon.
/// Projects where a live agent has a step in progress. A connected agent idle
/// at its prompt does not make a project "working".
fn working_count(state: &State) -> usize {
    state
        .projects
        .values()
        .filter(|project| {
            !project.summary.archived
                && project.features.values().any(|feature| {
                    feature.progress == agendar_core::model::Progress::Doing
                        && feature.liveness == agendar_core::model::state::Liveness::Live
                })
        })
        .count()
}

/// Items that need the person now: snoozed and dismissed ones are hidden from
/// Needs you, so they never turn the menu bar glyph amber either.
fn needs_count(
    state: &State,
    preferences: &crate::preferences::Preferences,
    now: UtcTime,
) -> usize {
    state
        .projects
        .values()
        .flat_map(|project| project.needs_you.iter())
        .filter(|need| {
            !preferences.dismissed.contains(&need.id)
                && preferences
                    .snoozed
                    .get(&need.id)
                    .is_none_or(|until| *until <= now)
        })
        .count()
}

fn changed_event(
    inner: &Inner,
    state: &State,
    events: &[Event],
    generation: u64,
    now: UtcTime,
    hint: &str,
) -> Value {
    let mut projects: Vec<&str> = state.projects.keys().map(ProjectSlug::as_str).collect();
    projects.sort_unstable();
    let host = inner.service.as_ref().map(|service| service.host().clone());
    let mut new_events: Vec<&Event> = events
        .iter()
        .filter(|event| !inner.seen_events.contains(&event.id))
        .collect();
    new_events.sort_by(|left, right| {
        (left.ts.canonical(), left.id.as_str()).cmp(&(right.ts.canonical(), right.id.as_str()))
    });
    // One entry per newly ingested pulse or progress report, carrying that
    // event's own timestamp and host: never a fabricated "just now".
    let mut pulses: Vec<Value> = Vec::new();
    for event in &new_events {
        if !matches!(
            event.payload,
            EventPayload::Pulse(_) | EventPayload::WorkProgress(_)
        ) {
            continue;
        }
        let Some(node) = &event.refs.node else {
            // A node-less session pulse has nothing to blip on the graph.
            continue;
        };
        pulses.push(json!({
            "id": event.id,
            "project": event.project,
            "node": node,
            "host": event.writer.host(),
            "event_ts": event.ts,
        }));
    }
    // `local` for this host's writes, `sync` when another host's events arrived.
    // A quiet clock tick stays `time` so nothing animates.
    let batch = if new_events.is_empty() {
        if hint == "time" { "time" } else { hint }
    } else if host
        .as_ref()
        .is_some_and(|host| new_events.iter().any(|event| event.writer.host() != host))
    {
        "sync"
    } else {
        "local"
    };
    json!({
        "v": 1,
        "epoch": inner.epoch,
        "generation": generation,
        "event_set_digest": state.event_set_digest,
        "as_of": now,
        "projects": projects,
        "pulses": pulses,
        "batch": batch,
    })
}

/// Record what this host has already published, without emitting anything.
fn remember(inner: &mut Inner, state: &State, events: &[Event], seed_unblocks: bool) {
    for event in events {
        inner.seen_events.insert(event.id.clone());
    }
    if seed_unblocks {
        for project in state.projects.values() {
            for decision in project.decisions.values() {
                if let Some(receipt) = &decision.effective_receipt
                    && project
                        .receipts
                        .get(receipt)
                        .is_some_and(|receipt| receipt.source == ReceiptSource::Human)
                {
                    inner
                        .unblocked
                        .insert(format!("answered:{}", receipt.as_str()));
                }
                if !decision.ack_history.is_empty()
                    && let Some(answer) = &decision.applied_answer
                {
                    inner.unblocked.insert(format!("acked:{}", answer.as_str()));
                }
            }
        }
    }
    inner.previous_state = Some(state.clone());
}

/// The `agendar://unblock` events a publication implies, each emitted once.
fn unblock_events(inner: &mut Inner, state: &State) -> Vec<Value> {
    let mut events: Vec<Value> = Vec::new();
    let previous = inner.previous_state.clone();
    for (slug, project) in &state.projects {
        let empty = agendar_core::model::ProjectState {
            summary: project.summary.clone(),
            features: BTreeMap::new(),
            decisions: BTreeMap::new(),
            receipts: BTreeMap::new(),
            deliveries: BTreeMap::new(),
            rules: Vec::new(),
            policy: project.policy.clone(),
            sessions: BTreeMap::new(),
            needs_you: Vec::new(),
            reports: Vec::new(),
        };
        let before = previous
            .as_ref()
            .and_then(|previous| previous.projects.get(slug))
            .unwrap_or(&empty);
        for decision in project.decisions.values() {
            let released = released_nodes(before, project, &decision.node);
            if let Some(receipt) = &decision.effective_receipt
                && project
                    .receipts
                    .get(receipt)
                    .is_some_and(|receipt| receipt.source == ReceiptSource::Human)
            {
                let key = format!("answered:{}", receipt.as_str());
                if inner.unblocked.insert(key) {
                    inner.releases.insert(receipt.clone(), released.clone());
                    events.push(unblock_value(
                        receipt,
                        slug,
                        &decision.id,
                        &released,
                        "answered",
                    ));
                }
            }
            // One `acked` phase per answered decision, whatever session or how
            // many ack events follow it.
            if decision.ack_history.is_empty() {
                continue;
            }
            let Some(answer) = decision.applied_answer.clone() else {
                continue;
            };
            let key = format!("acked:{}", answer.as_str());
            if !inner.unblocked.insert(key) {
                continue;
            }
            let released = inner.releases.get(&answer).cloned().unwrap_or_default();
            events.push(unblock_value(
                &answer,
                slug,
                &decision.id,
                &released,
                "acked",
            ));
        }
    }
    events
}

fn unblock_value(
    answer_id: &EventId,
    project: &ProjectSlug,
    ask: &EventId,
    released: &[String],
    phase: &str,
) -> Value {
    json!({
        "v": 1,
        "answer_id": answer_id,
        "project": project.as_str(),
        "from": format!("decision:{}", ask.as_str()),
        "released": released,
        "phase": phase,
    })
}

fn released_nodes(
    before: &agendar_core::model::ProjectState,
    after: &agendar_core::model::ProjectState,
    node: &agendar_core::model::NodePath,
) -> Vec<String> {
    // The answer releases the feature it gated and every feature downstream of
    // it; a later release of the same answer cannot be recovered from a
    // dependency test alone, so the gate transition is the fact that counts.
    let affected = affected_nodes(after, node);
    let mut released = Vec::new();
    for (path, feature) in &after.features {
        if !affected.contains(path) {
            continue;
        }
        let was_open = before
            .features
            .get(path)
            .is_some_and(|previous| previous.gate == agendar_core::model::Gate::Open);
        if was_open {
            continue;
        }
        if feature.gate == agendar_core::model::Gate::Open && feature.waits_on.is_empty() {
            released.push(path.as_str().to_owned());
        }
    }
    released.sort();
    released
}

/// `node` plus every feature reachable through accepted dependency edges.
fn affected_nodes(
    project: &agendar_core::model::ProjectState,
    node: &agendar_core::model::NodePath,
) -> BTreeSet<agendar_core::model::NodePath> {
    let mut adjacency: BTreeMap<
        agendar_core::model::NodePath,
        BTreeSet<agendar_core::model::NodePath>,
    > = BTreeMap::new();
    for (path, feature) in &project.features {
        for upstream in &feature.accepted_depends_on {
            adjacency
                .entry(upstream.clone())
                .or_default()
                .insert(path.clone());
        }
    }
    let mut affected: BTreeSet<agendar_core::model::NodePath> =
        agendar_core::reduce::graph::descendants(&adjacency, node)
            .into_iter()
            .collect();
    affected.insert(node.clone());
    affected
}

fn find_decision<'a>(
    state: &'a State,
    ask: &EventId,
) -> Option<(&'a ProjectSlug, &'a DecisionState)> {
    for (slug, project) in &state.projects {
        if let Some(decision) = project.decisions.get(ask) {
            return Some((slug, decision));
        }
    }
    None
}

fn decide_receipt(decision: &DecisionState) -> Option<EventId> {
    decision.effective_receipt.clone()
}

pub fn config_revision(config: &Config) -> Sha256 {
    projection::revision(json!(["config", config.revision, config.vault]))
}

/// Point the shared service at the bundled CLI when it is available. The app
/// itself never runs the CLI for an action; this only makes printed prefixes
/// absolute. Tests and development fall back to the process default.
fn apply_bundled_cli(service: &mut Service) {
    if let Ok(path) = bundled_cli_path() {
        service.set_cli_path(path);
    }
}

/// The non-canonical activity feed and header statistics of one project (UI
/// only), from one read of its recent events. A failed index read yields an
/// empty feed and statistics from the summary alone; the snapshot still loads.
fn project_extras(
    index: &Index,
    project: &ProjectState,
    now: UtcTime,
    waiting: &BTreeSet<EventId>,
    tokens: Option<agendar_core::runtime::usage::TokenUsage>,
) -> projection::ProjectExtras {
    let events = recent_project_events(
        index,
        &project.summary.slug,
        activity::ACTIVITY_EVENT_WINDOW,
    )
    .unwrap_or_default();
    let mut stats = project_stats::build(project, &events, now);
    stats.tokens = tokens;
    projection::ProjectExtras {
        activity: activity::build(project, &events),
        stats,
        listening: agendar_core::reduce::attention::listening(project, waiting),
    }
}

/// Hand a file to the platform's default application (a browser for html).
fn open_with_system(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// The linked repositories of `project` on this computer.
fn project_roots(config: &Config, project: &ProjectSlug) -> Vec<PathBuf> {
    config
        .repos
        .iter()
        .filter(|(_, slug)| *slug == project)
        .map(|(path, _)| PathBuf::from(path))
        .collect()
}

/// Activity, statistics and listening state of every project, keyed by slug.
fn project_extras_all(
    index: &Index,
    state: &State,
    now: UtcTime,
    waiting: &BTreeSet<EventId>,
    tokens: &dyn Fn(&ProjectSlug) -> Option<agendar_core::runtime::usage::TokenUsage>,
) -> BTreeMap<String, projection::ProjectExtras> {
    state
        .projects
        .iter()
        .map(|(slug, project)| {
            (
                slug.as_str().to_owned(),
                project_extras(index, project, now, waiting, tokens(slug)),
            )
        })
        .collect()
}

fn not_configured(inner: &Inner) -> AppError {
    if let Some(open_error) = &inner.open_error {
        return AppError::new(
            ErrorCode::Context.as_str(),
            format!(
                "The configured Vault cannot be opened: {}.",
                open_error.message()
            ),
        )
        .with_hint("Choose a valid Vault folder in setup.")
        .with_detail(
            "expected_config_revision",
            config_revision(&inner.config).as_str(),
        )
        .with_detail(
            "expected_preferences_revision",
            inner.preferences.wire_revision().as_str(),
        );
    }
    AppError::new(
        ErrorCode::Context.as_str(),
        "Agendar is not configured for a Vault yet.",
    )
    .with_hint("Choose a Vault folder in setup.")
    .with_detail(
        "expected_config_revision",
        config_revision(&inner.config).as_str(),
    )
    .with_detail(
        "expected_preferences_revision",
        inner.preferences.wire_revision().as_str(),
    )
}

fn stale_answer() -> AppError {
    AppError::new(
        ErrorCode::StaleAnswer.as_str(),
        "The decision changed after it was prepared.",
    )
    .with_hint("Review the refreshed card and prepare the answer again.")
}

/// The local config changed after Settings read it (another window or the
/// CLI). Settings re-reads it and shows its own words for this code.
fn stale_config_error() -> AgendarError {
    AgendarError::new(
        ErrorCode::StaleAnswer,
        "Settings on this computer changed after this page read them.",
    )
    .with_hint("Review the refreshed values and try again.")
}

fn request_conflict() -> AppError {
    AppError::new(
        ErrorCode::RequestConflict.as_str(),
        "This request id was already used with different input.",
    )
    .with_hint("Use a new request id for a different action.")
}

fn required_str<'a>(request: &'a Value, name: &str) -> ApiResult<&'a str> {
    request
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::validation(format!("/{name}"), format!("{name} is required.")))
}

fn optional_str<'a>(request: &'a Value, name: &str) -> Option<&'a str> {
    request.get(name).and_then(Value::as_str)
}

fn parse_done_when(
    value: Option<&Value>,
) -> ApiResult<Option<Vec<agendar_core::model::Criterion>>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| {
                AppError::validation("/done_when", format!("Invalid criteria: {error}."))
            }),
    }
}

fn parse_eta_field(value: Option<&Value>) -> ApiResult<Option<agendar_core::model::EtaRange>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| {
                AppError::validation("/expected_eta", format!("Invalid ETA: {error}."))
            }),
    }
}

/// Resolve the bundled CLI next to the running app executable. The staging
/// path is never used at runtime; `AGENDAR_CLI_PATH`
/// exists for development and tests.
pub fn bundled_cli_path() -> ApiResult<PathBuf> {
    if let Some(value) = std::env::var_os("AGENDAR_CLI_PATH") {
        let path = PathBuf::from(value);
        return path.canonicalize().map_err(|error| {
            AppError::new(
                ErrorCode::Context.as_str(),
                format!("AGENDAR_CLI_PATH does not resolve: {error}."),
            )
            .with_detail("cli_path", "AGENDAR_CLI_PATH")
        });
    }
    let name = if cfg!(windows) {
        "agendar.exe"
    } else {
        "agendar"
    };
    let exe = std::env::current_exe().map_err(|error| {
        AppError::new(
            ErrorCode::Io.as_str(),
            format!("Cannot resolve the app executable: {error}."),
        )
    })?;
    let candidate = exe
        .parent()
        .map(|parent| parent.join(name))
        .unwrap_or_else(|| PathBuf::from(name));
    candidate.canonicalize().map_err(|error| {
        AppError::new(
            ErrorCode::Context.as_str(),
            format!("The bundled agendar CLI was not found next to the app: {error}."),
        )
        .with_hint(
            "Rebuild with `pnpm -C app run bundle:cli`, or set AGENDAR_CLI_PATH for development.",
        )
        .with_detail("cli_path", candidate.to_string_lossy())
    })
}
