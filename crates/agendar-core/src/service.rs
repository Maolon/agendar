//! Shared validated read/mutation service used by the CLI (and later the app).
//!
//! The service is the only place that turns command input into events. It
//! reloads the Vault, applies current-state admission rules, appends through the
//! existing locked writer, and keeps a local operation journal so a request ID is
//! idempotent. Read commands ingest first unless the caller explicitly asks for
//! cached output.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::adapters::{AdapterCapabilities, capabilities_for};
use crate::error::{AgendarError, ErrorCode, Result};
use crate::index::{
    Index,
    ingest::ingest_until,
    open_index_until,
    query::{Query, QueryResult, RuleRecord, query},
    rebuild,
};
use crate::model::{
    Agent, CheckResult, Claim, CriterionResult, DecisionAck, DecisionAsk, DecisionCard,
    DecisionLifecycle, DecisionWithdrawn, Delivered, DeliveredOutcome, Delivery, EventId,
    EventPayload, EvidenceObserved, Gate, Harness, InboxPacket, Mode, NodePath, PacketKind,
    PlanFeature, PolicyEvaluation, PolicySet, Progress, ProjectDeclared, ProjectSlug, ProjectState,
    Refs, ReviewResult, ReviewVerdict, RuleSet, SessionId, Sha256, State, UtcTime, WorkProgress,
    WorkStatus, WriterId, WriterRole, canonical_json, validate_card_input,
};
use crate::reduce::{
    ReduceContext, Resolution, attention, budget, policy as policy_rules, reduce_resolved,
};
use crate::runtime::Config;
use crate::runtime::paths::display_path;
use crate::runtime::sessions::{
    DeliveryAttempt, LocalSession, OperationRecord, OperationStatus, RuntimeState, WaitLease,
    new_event_id, update_runtime, update_runtime_until,
};
use crate::runtime::watch::{RECONCILE_INTERVAL, VaultWatch, WaitOutcome};
use crate::runtime::{HostIdentity, LocalPaths};
use crate::vault::{
    AppendResult, Clock, EventDraft as VaultDraft, SystemClock, UlidIdSource, Vault, WriterKey,
    append_event_reserved,
};

/// Freshness metadata attached to every CLI read/mutation envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Freshness {
    pub evaluated_at: UtcTime,
    pub event_set_digest: Sha256,
    pub cached: bool,
}

/// One converged read of the Vault.
pub struct Fresh {
    pub index: Index,
    pub state: State,
    pub events: Vec<crate::model::Event>,
    pub resolution: Resolution,
    pub generation: u64,
    pub now: UtcTime,
}

/// Result of `init`.
#[derive(Debug, Clone, Serialize)]
pub struct InitResult {
    pub vault_id: crate::model::VaultId,
    pub host: crate::model::HostId,
    pub created: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
}

/// Result of `ask`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskResult {
    pub ask_id: EventId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<EventId>,
    pub effective_mode: Mode,
    pub waiting_on: Vec<EventId>,
    pub runnable_nodes: Vec<NodePath>,
    pub should_wait: bool,
    pub policy: PolicyEvaluation,
}

/// One unapplied answer for this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitingAnswer {
    pub ask: EventId,
    pub answer: EventId,
    /// Auto/human receipts require an ack; an assumed receipt is applied without
    /// one.
    pub needs_ack: bool,
}

/// The session facts the Claude hook shim inspects.
#[derive(Debug, Clone)]
pub struct LoopStatus {
    pub pending_asks: Vec<EventId>,
    pub waiting_answers: Vec<WaitingAnswer>,
    pub runnable: Vec<NodePath>,
    pub rules: Vec<RuleRecord>,
    pub packets: Vec<InboxPacket>,
}

/// Options for one blocked `wait`.
#[derive(Debug, Clone)]
pub struct WaitOptions {
    /// Bounds only this invocation; `None` waits without a caller deadline.
    pub timeout: Option<Duration>,
    /// Absolute end of the caller's whole-operation budget. Every lock wait and
    /// lease update inside the wait stays inside it; `None` means the timeout
    /// alone bounds the call.
    pub deadline: Option<Instant>,
    /// Slow reconciliation interval (60 s by default).
    pub reconcile: Duration,
    /// Wait-lease renewal interval (30 s by default).
    pub lease_renew: Duration,
    /// Cooperative cancellation set by the signal handler.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            timeout: None,
            deadline: None,
            reconcile: RECONCILE_INTERVAL,
            lease_renew: Duration::from_secs(30),
            cancel: None,
        }
    }
}

/// The bounded pending outcome of a wait, also returned when the whole
/// operation budget ran out before a lock could be taken.
fn wait_timeout() -> AgendarError {
    AgendarError::new(
        ErrorCode::Timeout,
        "No answer became available before the wait timeout.",
    )
    .with_detail("wait_timeout")
    .with_hint("Call wait again; the ask remains open.")
    .with_retryable(true)
}

/// Instrumentation hook for the no-spin wait oracle. Production passes a no-op.
pub trait WaitObserver {
    fn observed(&mut self, event: WaitEvent);
}

/// Observable wait events, including every decision query with its reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitEvent {
    Subscribed,
    QueryInitial,
    QueryInvalidation,
    QueryReconciliation,
    QueryDeadline,
    LeaseRenewed,
    AnswerFound,
    Withdrawn,
    TimedOut,
    Cancelled,
}

/// No-op observer used by the CLI.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopWaitObserver;

impl WaitObserver for NoopWaitObserver {
    fn observed(&mut self, _event: WaitEvent) {}
}

/// The shared service.
pub struct Service {
    vault: Vault,
    local: LocalPaths,
    host: HostIdentity,
    /// Absolute path of the running `agendar` binary, used verbatim in every
    /// printed command instruction. Overridable for tests and the app bundle.
    cli_path: std::path::PathBuf,
}

/// A stable key for one derived reminder: an overdue step is reminded again
/// only after a new estimate passes too.
fn reminder_key(nudge: &crate::reduce::nudges::Nudge) -> String {
    format!(
        "{:?}:{}:{}:{}",
        nudge.kind,
        nudge.node.as_ref().map_or("", |node| node.as_str()),
        nudge
            .attempt
            .as_ref()
            .map_or("", |attempt| attempt.as_str()),
        nudge.due_at.map(|due| due.canonical()).unwrap_or_default(),
    )
}

/// What one look for the answer found.
enum Looked {
    Found(Box<InboxPacket>),
    NotYet,
    /// The index was busy for a moment; look again shortly.
    Busy,
}

/// How soon `wait` looks again after a busy lock.
const BUSY_RETRY: Duration = Duration::from_millis(500);

impl Service {
    /// Initialize a Vault and local state, or open the existing pair.
    pub fn init(vault_root: &Path, data_root: &Path) -> Result<(Self, bool)> {
        let local = LocalPaths::new(data_root)?;
        let clock = SystemClock;
        let mut ids = UlidIdSource;
        let vault = Vault::init(vault_root, &clock, &mut ids)?;
        let created = vault.created();
        local.validate_against_vault_root(vault.root())?;
        local.ensure_layout(vault.root(), vault.id())?;
        let host = HostIdentity::load_or_create(local.data_root(), clock.now())?;
        Ok((
            Self {
                vault,
                local,
                host,
                cli_path: default_cli_path(),
            },
            created,
        ))
    }

    /// Open an existing Vault plus local state. A missing identity is `E_CONTEXT`
    /// and tells the caller to run `init`.
    pub fn open(vault_root: &Path, data_root: &Path) -> Result<Self> {
        let local = LocalPaths::new(data_root)?;
        let vault = Vault::open(vault_root)?;
        Self::open_with_local(vault, local)
    }

    fn open_with_local(vault: Vault, local: LocalPaths) -> Result<Self> {
        local.validate_against_vault_root(vault.root())?;
        local.ensure_layout(vault.root(), vault.id())?;
        let host = HostIdentity::load(local.data_root())?.ok_or_else(|| {
            AgendarError::new(ErrorCode::Context, "Missing local host identity.")
                .with_detail("missing_identity")
                .with_hint(format!(
                    "Run `{} init` or supply --vault and --data-dir.",
                    cli_token()
                ))
        })?;
        Ok(Self {
            vault,
            local,
            host,
            cli_path: default_cli_path(),
        })
    }

    /// Override the absolute CLI path printed in instructions (the app bundle
    /// resolves its staged/bundled binary; tests may set an explicit path).
    pub fn set_cli_path(&mut self, path: impl Into<std::path::PathBuf>) {
        self.cli_path = path.into();
    }

    /// The absolute CLI path printed in every instruction.
    pub fn cli_path(&self) -> &Path {
        &self.cli_path
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    pub fn local(&self) -> &LocalPaths {
        &self.local
    }

    pub fn host(&self) -> &crate::model::HostId {
        &self.host.host
    }

    /// Load the local runtime journal.
    pub fn runtime(&self) -> Result<RuntimeState> {
        RuntimeState::load(&self.local, &self.vault)
    }

    /// Asks a local `agendar wait` is blocked on right now (a fresh lease).
    pub fn waiting_asks(&self, now: UtcTime) -> BTreeSet<EventId> {
        self.runtime()
            .map(|runtime| {
                runtime
                    .wait_leases
                    .values()
                    .filter(|lease| {
                        lease
                            .updated_at
                            .add_seconds(i64::try_from(lease.ttl_s).unwrap_or(i64::MAX))
                            >= now
                    })
                    .map(|lease| lease.ask.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Record the selected Vault/project in local config.
    pub fn select_config(&self, project: Option<&ProjectSlug>) -> Result<()> {
        Config::select(self.local.data_root(), Some(self.vault.root()), project)
    }

    /// Map a canonical repo root to a project for hook context.
    pub fn map_repo(&self, repo: &Path, project: &ProjectSlug) -> Result<()> {
        Config::map_repo(self.local.data_root(), repo, project)
    }

    /// Freshness for one read.
    pub fn freshness(fresh: &Fresh, cached: bool) -> Result<Freshness> {
        let digest = crate::index::meta(fresh.index.connection(), "event_set_digest")?
            .map(|text| Sha256::parse(&text))
            .transpose()?
            .unwrap_or_else(|| Sha256::from_bytes(b""));
        Ok(Freshness {
            evaluated_at: fresh.now,
            event_set_digest: digest,
            cached,
        })
    }

    /// Converge the index and reduce the event set at `now`.
    pub fn refresh(&self, now: UtcTime, cached: bool) -> Result<Fresh> {
        self.refresh_until(now, cached, None)
    }

    /// Refresh without waiting past `deadline` for the index locks, so a
    /// budgeted caller (the Stop hook) never extends its own deadline inside a
    /// refresh.
    pub fn refresh_until(
        &self,
        now: UtcTime,
        cached: bool,
        deadline: Option<Instant>,
    ) -> Result<Fresh> {
        let mut index = open_index_until(&self.local, &self.vault, deadline)?;
        if !cached {
            let _ = ingest_until(&self.vault, &mut index, &[], now, deadline)?;
        }
        let (events, resolution) = crate::index::load_resolution(&index)?;
        let state = reduce_resolved(&resolution, &events, &ReduceContext { as_of: now });
        let generation = crate::index::meta(index.connection(), "generation")?
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(Fresh {
            index,
            state,
            events,
            resolution,
            generation,
            now,
        })
    }

    /// The project state or a typed not-found error.
    pub fn project<'a>(&self, fresh: &'a Fresh, project: &ProjectSlug) -> Result<&'a ProjectState> {
        fresh.state.projects.get(project).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::NotFound,
                format!("Project {project} was not found."),
            )
            .with_detail("project_not_found")
            .with_hint("Declare the project or check the --project value.")
        })
    }

    /// A session that may receive a delivery. A lost/sealed Codex registration
    /// stays addressable by its durable thread id; every other
    /// session must still be a live local registration.
    fn delivery_session(&self, session: &SessionId, project: &ProjectSlug) -> Result<LocalSession> {
        let runtime = self.runtime()?;
        let registration = runtime.sessions.get(session).filter(|record| {
            &record.project == project
                && ((!record.lost && !record.sealed)
                    || (record.harness == Harness::Codex
                        && !record.harness_session_id.trim().is_empty()))
        });
        registration.cloned().ok_or_else(|| {
            AgendarError::new(
                ErrorCode::Context,
                format!(
                    "Session {} is not registered on this host.",
                    session.as_str()
                ),
            )
            .with_detail("session_not_registered")
            .with_hint(format!(
                "Run `{} init --harness <harness> --harness-session <id> --pid <pid> --repo <path>`.",
                cli_token()
            ))
        })
    }

    /// Ensure one session is locally registered for this project.
    pub fn require_session(
        &self,
        session: &SessionId,
        project: &ProjectSlug,
    ) -> Result<LocalSession> {
        let runtime = self.runtime()?;
        let registration = runtime.registered(session).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::Context,
                format!(
                    "Session {} is not registered on this host.",
                    session.as_str()
                ),
            )
            .with_detail("session_not_registered")
            .with_hint(format!(
                "Run `{} init --harness <harness> --harness-session <id> --pid <pid> --repo <path>`.",
                cli_token()
            ))
        })?;
        if &registration.project != project {
            return Err(AgendarError::new(
                ErrorCode::Context,
                "The session is registered for a different project.",
            )
            .with_detail("session_project_mismatch"));
        }
        Ok(registration.clone())
    }

    /// The harness capabilities for sessions on this host.
    pub fn capabilities_for(&self, harness: Harness) -> AdapterCapabilities {
        let running = self
            .runtime()
            .map(|runtime| {
                runtime
                    .sessions
                    .values()
                    .any(|session| session.harness == harness && !session.lost)
            })
            .unwrap_or(false);
        capabilities_for(harness, running)
    }

    fn steward_key(&self, project: &ProjectSlug) -> WriterKey {
        WriterKey {
            project: project.clone(),
            writer: WriterId::new(self.host.host.clone(), WriterRole::Steward),
        }
    }

    fn operator_key(&self, project: &ProjectSlug) -> WriterKey {
        WriterKey {
            project: project.clone(),
            writer: WriterId::new(self.host.host.clone(), WriterRole::Operator),
        }
    }

    fn append(
        &self,
        key: &WriterKey,
        draft: VaultDraft,
        reserved: Option<EventId>,
    ) -> Result<AppendResult> {
        let clock = SystemClock;
        let mut ids = UlidIdSource;
        append_event_reserved(
            &self.vault,
            &self.local,
            key,
            draft,
            reserved,
            &clock,
            &mut ids,
        )
    }

    fn digest_input(
        command: &str,
        project: &ProjectSlug,
        session: &SessionId,
        request_id: Option<&str>,
        input: &Value,
    ) -> Result<Sha256> {
        let value = json!({
            "command": command,
            "project": project,
            "session": session,
            "request_id": request_id,
            "input": input,
        });
        Ok(Sha256::from_bytes(canonical_json(&value).as_bytes()))
    }

    /// Append one event through the operation journal.
    ///
    /// With a request ID, an identical retry returns the stored result without
    /// appending; a different input under the same request ID is
    /// `E_REQUEST_CONFLICT`. A crashed reservation reuses its reserved event ID,
    /// so the writer returns the original event instead of a duplicate.
    #[allow(clippy::too_many_arguments)]
    fn run_operation(
        &self,
        command: &str,
        project: &ProjectSlug,
        session: &SessionId,
        writer: WriterId,
        draft: VaultDraft,
        request_id: Option<&str>,
        input: &Value,
        build: impl FnOnce(&AppendResult) -> Result<Value>,
    ) -> Result<Value> {
        let digest = Self::digest_input(command, project, session, request_id, input)?;
        // Agents pick readable keys; scope them to the session so two agents
        // choosing the same key never meet in the journal.
        let key = request_id.map(|id| format!("{session}/{id}"));
        if let Some(key) = &key {
            let existing = update_runtime(&self.local, &self.vault, |runtime| {
                if let Some(record) = runtime.operations.get(key) {
                    if record.input_digest != digest {
                        return Err(AgendarError::new(
                            ErrorCode::RequestConflict,
                            "Request id was reused with different input.",
                        )
                        .with_detail("request_conflict")
                        .with_hint("Use a fresh --request-id for a different command."));
                    }
                    if record.status == OperationStatus::Committed {
                        return Ok(Some(record.result.clone().unwrap_or(Value::Null)));
                    }
                    return Ok(Some(Value::Null));
                }
                let reserved = Some(new_event_id()?);
                runtime.operations.insert(
                    key.clone(),
                    OperationRecord {
                        command: command.to_owned(),
                        project: project.clone(),
                        session: session.clone(),
                        writer: writer.to_string(),
                        input_digest: digest.clone(),
                        reserved_event: reserved,
                        draft: draft.clone(),
                        status: OperationStatus::Reserved,
                        result: None,
                        updated_at: SystemClock.now(),
                    },
                );
                Ok(None)
            })?;
            if let Some(stored) = existing.0
                && stored != Value::Null
            {
                return Ok(stored);
            }
            // Read the durable reservation (it may belong to another process,
            // which may already have committed it while we waited).
            let ((status, reserved, stored_draft, stored_result), _) =
                update_runtime(&self.local, &self.vault, |runtime| {
                    let record = runtime.operations.get(key).ok_or_else(|| {
                        AgendarError::new(ErrorCode::Io, "The operation reservation disappeared.")
                            .with_detail("operation_missing")
                    })?;
                    if record.input_digest != digest {
                        return Err(AgendarError::new(
                            ErrorCode::RequestConflict,
                            "Request id was reused with different input.",
                        )
                        .with_detail("request_conflict"));
                    }
                    Ok((
                        record.status,
                        record.reserved_event.clone(),
                        record.draft.clone(),
                        record.result.clone(),
                    ))
                })?;
            if status == OperationStatus::Committed {
                return Ok(stored_result.unwrap_or(Value::Null));
            }
            let draft = stored_draft;
            let result = self.append(
                &WriterKey {
                    project: project.clone(),
                    writer: writer.clone(),
                },
                draft,
                reserved,
            )?;
            let value = build(&result)?;
            let committed = update_runtime(&self.local, &self.vault, |runtime| {
                let Some(record) = runtime.operations.get_mut(key) else {
                    return Ok(value.clone());
                };
                if record.status == OperationStatus::Committed {
                    // Another process committed first; its result is the one a
                    // retry must observe.
                    return Ok(record.result.clone().unwrap_or(Value::Null));
                }
                record.status = OperationStatus::Committed;
                record.result = Some(value.clone());
                record.updated_at = SystemClock.now();
                Ok(value.clone())
            })?;
            return Ok(committed.0);
        }

        let key = WriterKey {
            project: project.clone(),
            writer,
        };
        let result = self.append(&key, draft, None)?;
        build(&result)
    }

    /// `project declare`.
    pub fn declare_project(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        mut patch: ProjectDeclared,
        request_id: Option<&str>,
    ) -> Result<Value> {
        self.require_session(session, project)?;
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        if patch.agent.is_some() {
            return Err(AgendarError::validation_at(
                "/agent",
                "client_agent",
                "The CLI supplies the agent identity; do not send it.",
            ));
        }
        let first = !fresh.resolution.ordered.iter().any(|event| {
            &event.project == project && matches!(event.payload, EventPayload::ProjectDeclared(_))
        });
        if first && (patch.title.is_none() || patch.brief.is_none() || patch.goal.is_none()) {
            return Err(AgendarError::validation_at(
                "/title",
                "first_declaration",
                "The first project declaration requires title, brief and goal.",
            ));
        }
        let registration = self.require_session(session, project)?;
        patch.agent = Some(Agent {
            session: session.clone(),
            harness: registration.harness,
        });
        let draft = VaultDraft {
            refs: Refs::default(),
            payload: EventPayload::ProjectDeclared(patch.clone()),
        };
        let input = json!({"project": project, "patch": patch});
        self.run_operation(
            "project declare",
            project,
            session,
            WriterId::new(self.host.host.clone(), WriterRole::Agent(session.clone())),
            draft,
            request_id,
            &input,
            |appended| {
                Ok(json!({
                    "event_id": appended.event.id,
                    "project": project,
                }))
            },
        )
    }

    /// `feature declare`.
    pub fn declare_feature(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        node: NodePath,
        mut patch: PlanFeature,
        request_id: Option<&str>,
    ) -> Result<Value> {
        let registration = self.require_session(session, project)?;
        if patch.agent.is_some() {
            return Err(AgendarError::validation_at(
                "/agent",
                "client_agent",
                "The CLI supplies the agent identity; do not send it.",
            ));
        }
        if patch.paused.is_some() || patch.handoff.is_some() {
            return Err(AgendarError::validation_at(
                "/paused",
                "human_only_field",
                "paused and handoff are human-owned fields.",
            ));
        }
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let first = !state.features.contains_key(&node);
        if first && (patch.title.is_none() || patch.slice.is_none() || patch.demo_kind.is_none()) {
            return Err(AgendarError::validation_at(
                "/title",
                "first_declaration",
                "The first feature declaration requires title, slice and demo_kind.",
            ));
        }
        if let Some(depends_on) = &patch.depends_on {
            self.reject_local_cycle(state, &node, depends_on)?;
        }
        patch.agent = Some(Agent {
            session: session.clone(),
            harness: registration.harness,
        });
        let draft = VaultDraft {
            refs: Refs {
                node: Some(node.clone()),
                ..Refs::default()
            },
            payload: EventPayload::PlanFeature(patch.clone()),
        };
        let input = json!({"node": node, "patch": patch});
        self.run_operation(
            "feature declare",
            project,
            session,
            WriterId::new(self.host.host.clone(), WriterRole::Agent(session.clone())),
            draft,
            request_id,
            &input,
            |appended| {
                Ok(json!({
                    "event_id": appended.event.id,
                    "node": node,
                    "rejected_edges": [],
                }))
            },
        )
    }

    /// Reject a local dependency submission that would close a cycle before any
    /// event is appended.
    fn reject_local_cycle(
        &self,
        state: &ProjectState,
        node: &NodePath,
        depends_on: &[NodePath],
    ) -> Result<()> {
        if depends_on.iter().any(|upstream| upstream == node) {
            return Err(AgendarError::validation_at(
                "/depends_on",
                "dependency_cycle",
                "A feature cannot depend on itself.",
            ));
        }
        let mut adjacency: BTreeMap<NodePath, BTreeSet<NodePath>> = BTreeMap::new();
        for (feature_node, feature) in &state.features {
            if feature_node == node {
                continue;
            }
            for upstream in &feature.accepted_depends_on {
                adjacency
                    .entry(upstream.clone())
                    .or_default()
                    .insert(feature_node.clone());
            }
        }
        for upstream in depends_on {
            if !state.features.contains_key(upstream) {
                // A missing upstream is blocked, not a fabricated cycle.
                continue;
            }
            if reachable(&adjacency, node, upstream).unwrap_or(false) {
                return Err(AgendarError::validation_at(
                    "/depends_on",
                    "dependency_cycle",
                    format!("Adding {upstream} → {node} would close a dependency cycle."),
                ));
            }
        }
        Ok(())
    }

    /// Resolve one attention/rescue row with a human-owned plan patch.
    ///
    /// Handoff records intent; it never starts, resumes or stops a process. The
    /// next session may start the feature only because the handoff event is a
    /// causal cause of its start.
    pub fn resolve_rescue(
        &self,
        project: &ProjectSlug,
        item_id: &str,
        action: crate::model::RescueAction,
    ) -> Result<Value> {
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let need = state
            .needs_you
            .iter()
            .find(|need| need.id == item_id)
            .ok_or_else(|| {
                AgendarError::new(ErrorCode::StaleAnswer, "This rescue is no longer current.")
                    .with_detail("stale_rescue")
                    .with_hint("Refresh the board before acting.")
            })?;
        let node = need.node.clone().ok_or_else(|| {
            AgendarError::validation_at(
                "/item_id",
                "rescue_without_node",
                "This rescue has no feature to hand off or pause.",
            )
        })?;
        if need.kind != crate::model::NeedsKind::Rescue {
            return Err(AgendarError::validation_at(
                "/item_id",
                "not_a_rescue",
                "This attention row is not a rescue action.",
            ));
        }
        if !state.features.contains_key(&node) {
            return Err(AgendarError::new(
                ErrorCode::NotFound,
                format!("Feature {} was not found.", node.as_str()),
            )
            .with_detail("feature_not_found"));
        }
        let draft = VaultDraft {
            refs: Refs {
                node: Some(node.clone()),
                causes: vec![need.source.clone()],
                ..Refs::default()
            },
            payload: EventPayload::PlanFeature(match action {
                crate::model::RescueAction::Handoff => PlanFeature {
                    handoff: Some(true),
                    ..PlanFeature::default()
                },
                crate::model::RescueAction::Pause => PlanFeature {
                    paused: Some(true),
                    ..PlanFeature::default()
                },
            }),
        };
        let appended = self.append(&self.operator_key(project), draft, None)?;
        Ok(json!({
            "event_id": appended.event.id,
            "node": node,
            "action": match action {
                crate::model::RescueAction::Handoff => "handoff",
                crate::model::RescueAction::Pause => "pause",
            },
        }))
    }

    /// Validate that a delivery addresses the current attempt and covers exactly
    /// the receipt's done_when criteria. This runs before any file upload.
    pub fn validate_delivery_target(
        &self,
        project: &ProjectSlug,
        node: &NodePath,
        attempt: &EventId,
        receipt_id: &EventId,
        criteria: &[crate::model::CriterionId],
    ) -> Result<()> {
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let receipt = state.receipts.get(receipt_id).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::NotFound,
                format!("Receipt {} was not found.", receipt_id.as_str()),
            )
            .with_detail("receipt_not_found")
        })?;
        if &receipt.node != node {
            return Err(AgendarError::validation_at(
                "/receipt",
                "receipt_node_mismatch",
                "The receipt belongs to a different feature.",
            ));
        }
        let decision = state.decisions.get(&receipt.ask).ok_or_else(|| {
            AgendarError::new(ErrorCode::NotFound, "The receipt's decision was not found.")
                .with_detail("decision_not_found")
        })?;
        if decision.effective_receipt.as_ref() != Some(receipt_id) {
            return Err(
                AgendarError::new(ErrorCode::StaleAnswer, "Answer is no longer current.")
                    .with_detail("stale_receipt")
                    .with_hint("Read inbox before delivering against this receipt."),
            );
        }
        let feature = state.features.get(node).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::NotFound,
                format!("Feature {} was not found.", node.as_str()),
            )
            .with_detail("feature_not_found")
        })?;
        if feature.attempt.as_ref() != Some(attempt) {
            return Err(AgendarError::validation_at(
                "/attempt",
                "stale_attempt",
                "A delivery must reference the feature's current attempt.",
            ));
        }
        let mut expected: Vec<&crate::model::CriterionId> = receipt
            .done_when
            .iter()
            .map(|criterion| &criterion.id)
            .collect();
        expected.sort();
        let mut supplied: Vec<&crate::model::CriterionId> = criteria.iter().collect();
        supplied.sort();
        if expected != supplied {
            return Err(AgendarError::validation_at(
                "/criteria",
                "delivery_criteria_mismatch",
                "The delivery must cover exactly the receipt's done_when criteria.",
            ));
        }
        Ok(())
    }

    /// `deliver`: validate one agent delivery against the current receipt and
    /// append it after every referenced blob is already published.
    #[allow(clippy::too_many_arguments)]
    pub fn deliver(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        node: &NodePath,
        attempt: &EventId,
        receipt_id: &EventId,
        supersedes: Option<&EventId>,
        mut body: Delivery,
        request_id: Option<&str>,
    ) -> Result<Value> {
        let registration = self.require_session(session, project)?;
        self.validate_delivery_target(
            project,
            node,
            attempt,
            receipt_id,
            &body
                .criteria
                .iter()
                .map(|criterion| criterion.criterion.clone())
                .collect::<Vec<_>>(),
        )?;
        let fresh = self.refresh(SystemClock.now(), false)?;
        let state = self.project(&fresh, project)?;
        for (index, criterion) in body.criteria.iter().enumerate() {
            if criterion.claim != Claim::AgentSays {
                return Err(AgendarError::validation_at(
                    format!("/criteria/{index}/claim"),
                    "delivery_seen_requires_later_observation",
                    "Delivery criteria must claim agent_says and omit observation.",
                ));
            }
            if criterion.observation.is_some() {
                return Err(AgendarError::validation_at(
                    format!("/criteria/{index}/observation"),
                    "delivery_observation_forbidden",
                    "Delivery criteria must omit observation.",
                ));
            }
        }
        if let Some(previous) = supersedes {
            let previous_delivery = state.deliveries.get(previous).ok_or_else(|| {
                AgendarError::new(
                    ErrorCode::NotFound,
                    format!("Delivery {} was not found.", previous.as_str()),
                )
                .with_detail("delivery_not_found")
            })?;
            if &previous_delivery.node != node {
                return Err(AgendarError::validation_at(
                    "/supersedes",
                    "delivery_node_mismatch",
                    "A delivery can only supersede a delivery on the same feature.",
                ));
            }
        }
        body.agent = Agent {
            session: session.clone(),
            harness: registration.harness,
        };
        let uploaded: Vec<crate::model::BlobRef> = body
            .demos
            .iter()
            .flat_map(demo_blobs)
            .chain(
                body.criteria
                    .iter()
                    .filter_map(|criterion| criterion.evidence.clone()),
            )
            .collect();
        let draft = VaultDraft {
            refs: Refs {
                node: Some(node.clone()),
                attempt: Some(attempt.clone()),
                receipt: Some(receipt_id.clone()),
                supersedes: supersedes.cloned(),
                ..Refs::default()
            },
            payload: EventPayload::Delivery(body.clone()),
        };
        let input = json!({
            "node": node,
            "attempt": attempt,
            "receipt": receipt_id,
            "supersedes": supersedes,
            "body": body,
        });
        self.run_operation(
            "deliver",
            project,
            session,
            WriterId::new(self.host.host.clone(), WriterRole::Agent(session.clone())),
            draft,
            request_id,
            &input,
            |appended| {
                Ok(json!({
                    "event_id": appended.event.id,
                    "review": "pending",
                    "uploaded": uploaded,
                }))
            },
        )
    }

    /// Append one Seen observation through the shared steward writer. The
    /// caller supplies the delivery's current observation frontier as causes so
    /// an explicit rerun can supersede older results without hiding history.
    pub fn append_observation(
        &self,
        project: &ProjectSlug,
        node: &NodePath,
        delivery: &EventId,
        body: EvidenceObserved,
        causes: Vec<EventId>,
    ) -> Result<EventId> {
        let draft = VaultDraft {
            refs: Refs {
                node: Some(node.clone()),
                delivery: Some(delivery.clone()),
                causes,
                ..Refs::default()
            },
            payload: EventPayload::EvidenceObserved(body),
        };
        let appended = self.append(&self.steward_key(project), draft, None)?;
        Ok(appended.event.id)
    }

    /// Append one human review, optionally followed by the independent house
    /// rule the human chose to save. Both events use the operator writer.
    #[allow(clippy::too_many_arguments)]
    pub fn review_delivery(
        &self,
        project: &ProjectSlug,
        node: &NodePath,
        delivery: &EventId,
        verdict: ReviewVerdict,
        reason: Option<String>,
        save_rule: Option<RuleSet>,
        causes: Vec<EventId>,
    ) -> Result<(EventId, Option<EventId>)> {
        let draft = VaultDraft {
            refs: Refs {
                node: Some(node.clone()),
                delivery: Some(delivery.clone()),
                causes,
                ..Refs::default()
            },
            payload: EventPayload::ReviewResult(ReviewResult {
                verdict,
                reason,
                save_rule: save_rule.clone(),
            }),
        };
        let appended = self.append(&self.operator_key(project), draft, None)?;
        let review_id = appended.event.id.clone();
        let rule_event = match save_rule {
            Some(rule) => Some(self.set_rule(project, rule, vec![review_id.clone()])?),
            None => None,
        };
        Ok((review_id, rule_event))
    }

    /// Append a human house rule. The reducer keeps the winning rule per id.
    pub fn set_rule(
        &self,
        project: &ProjectSlug,
        rule: RuleSet,
        causes: Vec<EventId>,
    ) -> Result<EventId> {
        let draft = VaultDraft {
            refs: Refs {
                causes,
                ..Refs::default()
            },
            payload: EventPayload::RuleSet(rule),
        };
        let appended = self.append(&self.operator_key(project), draft, None)?;
        Ok(appended.event.id)
    }

    /// `set_policy`: operator-only policy write with a revision guard.
    ///
    /// The frozen preset table, the irreversible lock and the budget bounds are
    /// re-validated here, so a frontend cannot lock the human out of a required
    /// ask.
    pub fn set_policy(
        &self,
        project: &ProjectSlug,
        policy: PolicySet,
        expected_revision: &Sha256,
    ) -> Result<EventId> {
        policy_rules::validate_policy(&policy)?;
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        if &state.policy.revision != expected_revision {
            return Err(stale_token(
                "The policy changed since it was displayed.",
                "stale_policy",
            ));
        }
        let draft = VaultDraft {
            refs: Refs::default(),
            payload: EventPayload::PolicySet(policy),
        };
        let appended = self.append(&self.operator_key(project), draft, None)?;
        Ok(appended.event.id)
    }

    /// `update_project`: human agenda fields with per-field revision guards.
    pub fn update_project(
        &self,
        project: &ProjectSlug,
        patch: ProjectDeclared,
        expected_fields: &BTreeMap<String, Sha256>,
    ) -> Result<EventId> {
        if patch.agent.is_some() {
            return Err(AgendarError::validation_at(
                "/agent",
                "client_agent",
                "The host supplies the writer identity; do not send an agent.",
            ));
        }
        validate_project_patch(&patch)?;
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let current = crate::model::state::project_field_revisions(project, &state.summary);
        check_expected(&patched_project_fields(&patch), expected_fields, &current)?;
        let draft = VaultDraft {
            refs: Refs::default(),
            payload: EventPayload::ProjectDeclared(patch),
        };
        let appended = self.append(&self.operator_key(project), draft, None)?;
        Ok(appended.event.id)
    }

    /// `update_feature`: human agenda fields with per-field and graph guards.
    pub fn update_feature(
        &self,
        project: &ProjectSlug,
        node: &NodePath,
        patch: PlanFeature,
        expected_fields: &BTreeMap<String, Sha256>,
        expected_graph_revision: Option<&Sha256>,
    ) -> Result<EventId> {
        if patch.agent.is_some() {
            return Err(AgendarError::validation_at(
                "/agent",
                "client_agent",
                "The host supplies the writer identity; do not send an agent.",
            ));
        }
        validate_feature_patch(&patch)?;
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let feature = state.features.get(node).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::NotFound,
                format!("Feature {} was not found.", node.as_str()),
            )
            .with_detail("feature_not_found")
        })?;
        let current = crate::model::state::feature_field_revisions(project, feature);
        check_expected(&patched_feature_fields(&patch), expected_fields, &current)?;
        if let Some(depends_on) = &patch.depends_on {
            let expected_graph = expected_graph_revision.ok_or_else(|| {
                AgendarError::validation_at(
                    "/expected_graph_revision",
                    "expected_graph_revision_required",
                    "A dependency edit must echo the current graph revision.",
                )
            })?;
            let graph_source = feature.field_sources.get("depends_on");
            let current_graph = crate::model::state::graph_revision(project, node, graph_source);
            if &current_graph != expected_graph {
                return Err(stale_token(
                    "The dependency plan changed since it was displayed.",
                    "stale_graph",
                ));
            }
            self.reject_local_cycle(state, node, depends_on)?;
        } else if expected_graph_revision.is_some() {
            return Err(AgendarError::validation_at(
                "/expected_graph_revision",
                "graph_revision_without_dependency",
                "expected_graph_revision only applies to a depends_on patch.",
            ));
        }
        let draft = VaultDraft {
            refs: Refs {
                node: Some(node.clone()),
                ..Refs::default()
            },
            payload: EventPayload::PlanFeature(patch),
        };
        let appended = self.append(&self.operator_key(project), draft, None)?;
        Ok(appended.event.id)
    }

    /// `progress started`.
    pub fn progress_started(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        node: &NodePath,
        eta: crate::model::EtaRange,
        note: Option<String>,
        request_id: Option<&str>,
    ) -> Result<Value> {
        self.progress_started_applying(project, session, node, eta, note, &[], request_id)
    }

    /// `progress started --applies <answer-id>...`: start work that applies
    /// answered questions on this step or upstream. The start cites them, which
    /// is what applying a receipt means, so an answered
    /// step no longer has to be acked before the work that applies it begins.
    #[allow(clippy::too_many_arguments)]
    pub fn progress_started_applying(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        node: &NodePath,
        eta: crate::model::EtaRange,
        note: Option<String>,
        applies: &[EventId],
        request_id: Option<&str>,
    ) -> Result<Value> {
        let registration = self.require_session(session, project)?;
        eta.validate("/eta")?;
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let feature = state.features.get(node).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::NotFound,
                format!("Feature {} was not found.", node.as_str()),
            )
            .with_detail("feature_not_found")
            .with_hint("Declare the feature before starting work.")
        })?;
        if !feature.complete {
            return Err(AgendarError::validation_at(
                "/node",
                "feature_incomplete",
                "The feature is missing its title, slice or demo kind.",
            ));
        }
        let terminal = matches!(
            feature.progress,
            Progress::Done | Progress::Verified | Progress::Dropped
        );
        for (index, answer) in applies.iter().enumerate() {
            if !state
                .decisions
                .values()
                .any(|decision| decision.effective_receipt.as_ref() == Some(answer))
            {
                return Err(AgendarError::validation_at(
                    format!("/applies/{index}"),
                    "not_a_current_answer",
                    format!("{answer} is not a current answer in this project."),
                ));
            }
        }
        let unapplied: Vec<EventId> = attention::unapplied_receipts(state, session, node)
            .into_iter()
            .filter(|receipt| !applies.contains(receipt))
            .collect();
        let repair = repair_restart(session, feature);
        let admitted = repair
            || if terminal {
                // A restart of this session's own finished work is allowed
                //; it is still gated by pause, waits and handoff.
                feature.session.as_ref() == Some(session)
                    && !feature.paused
                    && !feature.handoff_pending
                    && feature.waits_on.is_empty()
                    && feature.gate == Gate::Open
                    && feature.assumptions.is_empty()
                    && feature.taints.is_empty()
            } else {
                attention::runnable_except_receipts(session, node, state) && unapplied.is_empty()
            };
        if !admitted
            && !terminal
            && !unapplied.is_empty()
            && attention::runnable_except_receipts(session, node, state)
        {
            let ids: Vec<&str> = unapplied.iter().map(EventId::as_str).collect();
            return Err(AgendarError::new(
                ErrorCode::Policy,
                "An answer for this step is not applied yet.",
            )
            .with_detail("receipt_unapplied")
            .with_hint(format!(
                "Read it with inbox or wait, then start with --applies {} (or ack it first).",
                ids.join(" --applies ")
            )));
        }
        if !admitted {
            return Err(AgendarError::new(
                ErrorCode::Policy,
                "This feature is not runnable for this session.",
            )
            .with_detail("not_runnable")
            .with_hint("Resolve the open gate or wait for the current owner to finish."));
        }
        let previous_attempt = feature.attempt.clone();
        let mut causes: Vec<EventId> = previous_attempt.into_iter().collect();
        if feature.handoff_pending
            && let Some(handoff) = &feature.handoff_event
        {
            causes.push(handoff.clone());
        }
        // The service fills the causal basis the reducer derives assumptions
        // from: unresolved assumptions of this feature and its transitive
        // upstreams, plus the overriding answers a tainted feature must cite to
        // be repaired.
        let mut basis = start_basis(state, node);
        basis.extend(applies.iter().cloned());
        basis.sort();
        basis.dedup();
        let draft = VaultDraft {
            refs: Refs {
                node: Some(node.clone()),
                causes,
                basis: basis.clone(),
                ..Refs::default()
            },
            payload: EventPayload::WorkProgress(WorkProgress {
                agent: Agent {
                    session: session.clone(),
                    harness: registration.harness,
                },
                status: WorkStatus::Started,
                eta: Some(eta),
                evidence: Vec::new(),
                note: note.clone(),
            }),
        };
        let input = json!({"node": node, "eta": eta, "note": note, "basis": basis});
        self.run_operation(
            "progress started",
            project,
            session,
            WriterId::new(self.host.host.clone(), WriterRole::Agent(session.clone())),
            draft,
            request_id,
            &input,
            |appended| {
                Ok(json!({
                    "event_id": appended.event.id,
                    "attempt": appended.event.id,
                    "progress": "doing",
                }))
            },
        )
    }

    /// `progress done|dropped|verified`.
    #[allow(clippy::too_many_arguments)]
    pub fn progress_finished(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        node: &NodePath,
        attempt: &EventId,
        status: WorkStatus,
        evidence: Vec<CriterionResult>,
        note: Option<String>,
        request_id: Option<&str>,
    ) -> Result<Value> {
        if status == WorkStatus::Started {
            return Err(AgendarError::validation_at(
                "/status",
                "use_started",
                "Use `progress started` to start an attempt.",
            ));
        }
        let registration = self.require_session(session, project)?;
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let feature = state.features.get(node).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::NotFound,
                format!("Feature {} was not found.", node.as_str()),
            )
            .with_detail("feature_not_found")
        })?;
        if feature.attempt.as_ref() != Some(attempt) {
            return Err(AgendarError::validation_at(
                "/attempt",
                "attempt_not_current",
                "The attempt does not match the feature's current attempt.",
            ));
        }
        if feature.session.as_ref() != Some(session) {
            return Err(AgendarError::validation_at(
                "/node",
                "attempt_owner",
                "Only the session that owns the current attempt may report its progress.",
            ));
        }
        if status == WorkStatus::Verified
            && !evidence.iter().any(|result| {
                result.status == CheckResult::Passed
                    && (result.evidence.is_some() || result.observation.is_some())
            })
        {
            return Err(AgendarError::validation_at(
                "/evidence",
                "verified_requires_passed_evidence",
                "progress verified requires at least one passed result with a blob or observation.",
            ));
        }
        let draft = VaultDraft {
            refs: Refs {
                node: Some(node.clone()),
                attempt: Some(attempt.clone()),
                ..Refs::default()
            },
            payload: EventPayload::WorkProgress(WorkProgress {
                agent: Agent {
                    session: session.clone(),
                    harness: registration.harness,
                },
                status,
                eta: None,
                evidence: evidence.clone(),
                note: note.clone(),
            }),
        };
        let input = json!({
            "node": node,
            "attempt": attempt,
            "status": status,
            "evidence": evidence,
            "note": note,
        });
        let awaiting_review = state.policy.value.require_human_review;
        self.run_operation(
            "progress finished",
            project,
            session,
            WriterId::new(self.host.host.clone(), WriterRole::Agent(session.clone())),
            draft,
            request_id,
            &input,
            |appended| {
                let after = self.refresh(SystemClock.now(), false)?;
                let actual = after
                    .state
                    .projects
                    .get(project)
                    .and_then(|state| state.features.get(node))
                    .map(|feature| feature.progress);
                let projected = match actual {
                    Some(Progress::Verified) => "verified",
                    Some(Progress::Done) => "done",
                    Some(Progress::Dropped) => "dropped",
                    Some(Progress::Doing) => "doing",
                    Some(Progress::Planned) | None => match status {
                        WorkStatus::Verified if awaiting_review => "done",
                        WorkStatus::Verified => "verified",
                        WorkStatus::Done => "done",
                        WorkStatus::Dropped => "dropped",
                        WorkStatus::Started => "doing",
                    },
                };
                let mut data = json!({
                    "event_id": appended.event.id,
                    "progress": projected,
                });
                if status == WorkStatus::Verified {
                    data["awaiting_review"] = json!(awaiting_review && projected == "done");
                }
                Ok(data)
            },
        )
    }

    /// `progress dropped` without an attempt: a planned step nobody started
    /// leaves the plan. Any session registered in the project may drop it; a
    /// started step is dropped through its attempt instead, and a step with an
    /// open question needs that question withdrawn first. A later start brings
    /// the step back.
    pub fn drop_unstarted(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        node: &NodePath,
        note: Option<String>,
        request_id: Option<&str>,
    ) -> Result<Value> {
        let registration = self.require_session(session, project)?;
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let feature = state.features.get(node).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::NotFound,
                format!("Feature {} was not found.", node.as_str()),
            )
            .with_detail("feature_not_found")
        })?;
        if let Some(attempt) = &feature.attempt {
            return Err(AgendarError::validation_at(
                "/attempt",
                "attempt_required",
                "This step was started; drop its attempt instead.",
            )
            .with_hint(format!("Pass --attempt {attempt}.")));
        }
        if feature.progress == Progress::Dropped {
            return Err(AgendarError::validation_at(
                "/node",
                "already_dropped",
                "This step is already dropped.",
            ));
        }
        if state
            .decisions
            .values()
            .any(|decision| &decision.node == node && decision.lifecycle == DecisionLifecycle::Open)
        {
            return Err(
                AgendarError::new(ErrorCode::Policy, "This step has an open question.")
                    .with_detail("open_decision")
                    .with_hint("Withdraw the question first, then drop the step."),
            );
        }
        let draft = VaultDraft {
            refs: Refs {
                node: Some(node.clone()),
                ..Refs::default()
            },
            payload: EventPayload::WorkProgress(WorkProgress {
                agent: Agent {
                    session: session.clone(),
                    harness: registration.harness,
                },
                status: WorkStatus::Dropped,
                eta: None,
                evidence: Vec::new(),
                note: note.clone(),
            }),
        };
        let input = json!({"node": node, "status": "dropped", "note": note});
        self.run_operation(
            "progress dropped",
            project,
            session,
            WriterId::new(self.host.host.clone(), WriterRole::Agent(session.clone())),
            draft,
            request_id,
            &input,
            |appended| {
                Ok(json!({
                    "event_id": appended.event.id,
                    "progress": "dropped",
                }))
            },
        )
    }

    /// `pulse` with the non-boundary rate limit.
    #[allow(clippy::too_many_arguments)]
    pub fn pulse(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        node: &NodePath,
        attempt: &EventId,
        message: String,
        boundary: bool,
        eta: Option<crate::model::EtaRange>,
        request_id: Option<&str>,
    ) -> Result<Value> {
        let registration = self.require_session(session, project)?;
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let feature = state.features.get(node).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::NotFound,
                format!("Feature {} was not found.", node.as_str()),
            )
            .with_detail("feature_not_found")
        })?;
        if feature.attempt.as_ref() != Some(attempt) {
            return Err(AgendarError::validation_at(
                "/attempt",
                "attempt_not_current",
                "The attempt does not match the feature's current attempt.",
            ));
        }
        if feature.session.as_ref() != Some(session) {
            return Err(AgendarError::validation_at(
                "/node",
                "attempt_owner",
                "Only the session that owns the current attempt may pulse it.",
            ));
        }
        if !boundary {
            let last = fresh
                .resolution
                .ordered
                .iter()
                .filter(|event| &event.project == project)
                .filter_map(|event| match &event.payload {
                    EventPayload::Pulse(body)
                        if !body.boundary && &body.agent.session == session =>
                    {
                        Some(event.ts)
                    }
                    _ => None,
                })
                .next_back();
            if let Some(last) = last {
                let elapsed = now.millis().saturating_sub(last.millis());
                if elapsed < 600_000 {
                    let retry_after_s = ((600_000 - elapsed) as u64).div_ceil(1000);
                    return Ok(json!({
                        "event_id": Value::Null,
                        "rate_limited": true,
                        "retry_after_s": retry_after_s,
                    }));
                }
            }
        }
        let draft = VaultDraft {
            refs: Refs {
                node: Some(node.clone()),
                attempt: Some(attempt.clone()),
                ..Refs::default()
            },
            payload: EventPayload::Pulse(crate::model::Pulse {
                agent: Agent {
                    session: session.clone(),
                    harness: registration.harness,
                },
                message: message.clone(),
                boundary,
                eta,
            }),
        };
        let input = json!({
            "node": node,
            "attempt": attempt,
            "message": message,
            "boundary": boundary,
            "eta": eta,
        });
        self.run_operation(
            "pulse",
            project,
            session,
            WriterId::new(self.host.host.clone(), WriterRole::Agent(session.clone())),
            draft,
            request_id,
            &input,
            |appended| {
                Ok(json!({
                    "event_id": appended.event.id,
                    "rate_limited": false,
                }))
            },
        )
    }

    /// `ask`: validate everything, evaluate policy, then append one ask event.
    /// Nothing is written when any check fails.
    pub fn ask(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        node: &NodePath,
        card: DecisionCard,
        requested: Mode,
        request_id: Option<&str>,
    ) -> Result<AskResult> {
        validate_card_input(&card)?;
        let registration = self.require_session(session, project)?;
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let Some(feature) = state.features.get(node) else {
            return Err(AgendarError::new(
                ErrorCode::NotFound,
                format!("Feature {} was not found.", node.as_str()),
            )
            .with_detail("feature_not_found")
            .with_hint("Declare the feature before asking a decision."));
        };
        if !feature.complete {
            return Err(AgendarError::validation_at(
                "/node",
                "feature_incomplete",
                "The feature is missing its title, slice or demo kind.",
            ));
        }
        for (index, fact) in card.already_checked.iter().enumerate() {
            if fact.claim == Claim::Seen {
                if fact.observation.is_none() {
                    return Err(AgendarError::validation_at(
                        format!("/card/already_checked/{index}/observation"),
                        "seen_requires_observation",
                        "A Seen fact needs a real observation; label the fact Agent says.",
                    ));
                }
                return Err(AgendarError::validation_at(
                    format!("/card/already_checked/{index}/observation"),
                    "seen_observation_unavailable",
                    "Sync the observation or label the fact Agent says.",
                ));
            }
        }
        let policy_event =
            fresh
                .resolution
                .ordered
                .iter()
                .rev()
                .find_map(|event| match &event.payload {
                    EventPayload::PolicySet(_) if &event.project == project => {
                        Some(event.id.clone())
                    }
                    _ => None,
                });
        let evaluation = policy_rules::evaluate_with_policy(
            &card,
            requested,
            &state.policy.value,
            policy_event.clone(),
        );
        // Report the real derived usage, never the empty shell an agent could
        // have supplied; the claimed snapshot is advisory evidence only.
        let evaluation = PolicyEvaluation {
            budget: state.summary.budget.clone(),
            ..evaluation
        };
        let requires_hold = evaluation.reversibility == crate::model::Reversibility::Irreversible
            || evaluation.external
            || !state.policy.value.allow_timeouts;
        if requires_hold && !matches!(card.on_timeout, crate::model::OnTimeout::Hold) {
            return Err(AgendarError::new(
                ErrorCode::Policy,
                "This decision requires ask with on_timeout=hold.",
            )
            .with_field("/on_timeout")
            .with_detail("hold_required")
            .with_hint("Change on_timeout to {\"kind\":\"hold\"} or edit the card risk facts."));
        }
        if evaluation.effective != Mode::Ask {
            // A speculative admission is budget-checked before any assumed or
            // auto choice is written.
            let activity = budget::Activity {
                speculative_features: state.summary.budget.speculative_features,
                pending_slices: state.summary.budget.pending_slices,
                demo_gap_s: state.summary.budget.demo_gap_s,
            };
            budget::admit_speculation(
                &state.policy.value.budget,
                activity,
                node,
                &budget::speculative_nodes(&state.features, &state.decisions, &state.receipts),
            )?;
        }
        // The author's own unfinished attempt on this node, if any: a choice
        // binds to it and a choice without one must omit the reference
        //.
        let choice_attempt = state.features.get(node).and_then(|feature| {
            (feature.session.as_ref() == Some(session) && feature.progress == Progress::Doing)
                .then(|| feature.attempt.clone())
                .flatten()
        });
        let mut causes = Vec::new();
        if let Some(policy_event) = &policy_event {
            causes.push(policy_event.clone());
        }
        let draft = VaultDraft {
            refs: Refs {
                node: Some(node.clone()),
                causes,
                ..Refs::default()
            },
            payload: EventPayload::DecisionAsk(DecisionAsk {
                agent: Agent {
                    session: session.clone(),
                    harness: registration.harness,
                },
                card: card.clone(),
                policy: evaluation.clone(),
            }),
        };
        let input = json!({
            "node": node,
            "mode": requested,
            "card": card,
        });
        let value = self.run_operation(
            "ask",
            project,
            session,
            WriterId::new(self.host.host.clone(), WriterRole::Agent(session.clone())),
            draft,
            request_id,
            &input,
            |appended| {
                // An assumed or auto choice follows its ask causally and is
                // written before the command reports success.
                let receipt_id = if evaluation.effective == Mode::Ask {
                    None
                } else {
                    let agent = Agent {
                        session: session.clone(),
                        harness: registration.harness,
                    };
                    let choice = crate::model::DecisionChoice {
                        agent,
                        option: card.recommended.option.clone(),
                        reason: crate::model::ChoiceReason::Policy,
                        policy: evaluation.clone(),
                    };
                    let payload = match evaluation.effective {
                        Mode::Assume => EventPayload::DecisionAssumed(choice),
                        _ => EventPayload::DecisionAuto(choice),
                    };
                    let draft = VaultDraft {
                        refs: Refs {
                            node: Some(node.clone()),
                            ask: Some(appended.event.id.clone()),
                            attempt: choice_attempt.clone(),
                            ..Refs::default()
                        },
                        payload,
                    };
                    let chosen = self.append(
                        &WriterKey {
                            project: project.clone(),
                            writer: WriterId::new(
                                self.host.host.clone(),
                                WriterRole::Agent(session.clone()),
                            ),
                        },
                        draft,
                        None,
                    )?;
                    Some(chosen.event.id)
                };
                let after = self.refresh(now, false)?;
                let after_state = self.project(&after, project)?;
                let runnable = attention::runnable_nodes(session, after_state);
                let mut waiting_on: Vec<EventId> = after_state
                    .decisions
                    .values()
                    .filter(|decision| decision.lifecycle == DecisionLifecycle::Open)
                    .map(|decision| decision.id.clone())
                    .collect();
                waiting_on.sort();
                let result = AskResult {
                    ask_id: appended.event.id.clone(),
                    receipt_id,
                    effective_mode: evaluation.effective,
                    waiting_on,
                    should_wait: runnable.is_empty(),
                    runnable_nodes: runnable,
                    policy: evaluation.clone(),
                };
                serde_json::to_value(result).map_err(|error| {
                    AgendarError::new(
                        ErrorCode::Io,
                        format!("Cannot serialize the ask result: {error}."),
                    )
                    .with_detail("result_serialize")
                })
            },
        )?;
        serde_json::from_value(value).map_err(|error| {
            AgendarError::new(
                ErrorCode::Io,
                format!("Cannot decode the stored ask result: {error}."),
            )
            .with_detail("result_decode")
        })
    }

    /// `inbox`: deliver packets and rules. It never acknowledges anything.
    pub fn inbox(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        node: Option<&NodePath>,
        cached: bool,
    ) -> Result<(Vec<InboxPacket>, Vec<RuleRecord>, u64)> {
        self.require_session(session, project)?;
        let now = SystemClock.now();
        let fresh = self.refresh(now, cached)?;
        let packets = match query(
            &fresh.index,
            Query::Inbox {
                project: project.clone(),
                session: session.clone(),
                node: node.cloned(),
            },
            now,
        )? {
            QueryResult::Inbox { packets } => packets,
            _ => Vec::new(),
        };
        let rules = self.rules(project, cached)?;
        Ok((packets, rules, fresh.generation))
    }

    /// `rules`.
    pub fn rules(&self, project: &ProjectSlug, cached: bool) -> Result<Vec<RuleRecord>> {
        let now = SystemClock.now();
        let fresh = self.refresh(now, cached)?;
        match query(
            &fresh.index,
            Query::Rules {
                project: project.clone(),
            },
            now,
        )? {
            QueryResult::Rules { rules } => Ok(rules),
            _ => Ok(Vec::new()),
        }
    }

    /// `board`: everything an agent joining a project needs before it touches
    /// state. Whether the project is already declared, who works on what, which
    /// steps this session owns and which it may start. Read-only.
    pub fn board(
        &self,
        project: &ProjectSlug,
        session: Option<&SessionId>,
        cached: bool,
    ) -> Result<Value> {
        let now = SystemClock.now();
        let fresh = self.refresh(now, cached)?;
        let Some(state) = fresh.state.projects.get(project) else {
            return Ok(serde_json::json!({
                "project": {"slug": project, "declared": false},
                "features": [],
                "yours": [],
                "others": [],
                "runnable": [],
                "nudges": [],
                "reconcile": [],
            }));
        };
        let live = |id: &SessionId| {
            state
                .sessions
                .get(id)
                .is_some_and(|session| session.liveness == crate::model::state::Liveness::Live)
        };
        let features: Vec<Value> = state
            .features
            .values()
            .map(|feature| {
                let owner = feature
                    .session
                    .as_ref()
                    .and_then(|id| state.sessions.get(id))
                    .map(|owner| {
                        serde_json::json!({
                            "session": owner.id,
                            "harness": owner.harness,
                            "live": live(&owner.id),
                            "you": session == Some(&owner.id),
                        })
                    });
                serde_json::json!({
                    "node": feature.node,
                    "title": feature.title,
                    "progress": feature.progress,
                    "milestone": feature.milestone,
                    "gate": feature.gate,
                    "depends_on": feature.depends_on,
                    "owner": owner,
                })
            })
            .collect();
        let yours: Vec<&NodePath> = state
            .features
            .values()
            .filter(|feature| session.is_some() && feature.session.as_ref() == session)
            .map(|feature| &feature.node)
            .collect();
        let others: Vec<Value> = state
            .sessions
            .values()
            .filter(|other| {
                Some(&other.id) != session && other.liveness == crate::model::state::Liveness::Live
            })
            .map(|other| {
                serde_json::json!({
                    "session": other.id,
                    "harness": other.harness,
                    "current_node": other.current_node,
                })
            })
            .collect();
        let runnable = session
            .map(|id| crate::reduce::attention::runnable_nodes(id, state))
            .unwrap_or_default();
        // Open steps this agent should close, update or carry on: its own, the
        // ones nobody owns, and the ones whose owner is lost. A step another
        // agent still holds (live or quiet) is not listed.
        let mut reconcile: Vec<(bool, Value)> = state
            .features
            .values()
            .filter(|feature| matches!(feature.progress, Progress::Planned | Progress::Doing))
            .filter_map(|feature| {
                let holder = match feature.session.as_ref() {
                    None => "nobody",
                    Some(id) if Some(id) == session => "you",
                    Some(id)
                        if state.sessions.get(id).is_none_or(|owner| {
                            owner.liveness == crate::model::state::Liveness::Lost
                        }) =>
                    {
                        "lost"
                    }
                    Some(_) => return None,
                };
                Some((
                    feature.progress == Progress::Doing,
                    serde_json::json!({
                        "node": feature.node,
                        "title": feature.title,
                        "progress": feature.progress,
                        "gate": feature.gate,
                        "owner": holder,
                    }),
                ))
            })
            .collect();
        reconcile.sort_by_key(|(doing, _)| !doing);
        let reconcile: Vec<Value> = reconcile.into_iter().map(|(_, step)| step).collect();
        let nudges = session
            .map(|id| crate::reduce::nudges::for_session(state, id))
            .unwrap_or_default();
        Ok(serde_json::json!({
            "project": {
                "slug": state.summary.slug,
                "declared": !state.summary.goal.is_empty(),
                "title": state.summary.title,
                "goal": state.summary.goal,
            },
            "you": session.and_then(|id| state.sessions.get(id)).map(|me| serde_json::json!({
                "session": me.id,
                "current_node": me.current_node,
            })),
            "features": features,
            "yours": yours,
            "others": others,
            "runnable": runnable,
            "nudges": nudges,
            "reconcile": reconcile,
        }))
    }

    /// Human plan edits `session` has not been shown yet (goal, steps, rules,
    /// policy), and the newest of them for [`Service::mark_plan_seen`]. A
    /// session sees only edits made after it registered.
    pub fn plan_nudges(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        cached: bool,
    ) -> Result<(Vec<crate::reduce::nudges::Nudge>, Option<EventId>)> {
        let now = SystemClock.now();
        let fresh = self.refresh(now, cached)?;
        let runtime = self.runtime()?;
        let Some(local) = runtime.sessions.get(session) else {
            return Ok((Vec::new(), None));
        };
        let nudges = crate::reduce::nudges::plan_changes(
            &fresh.resolution.ordered,
            project,
            local.agenda_seen.as_ref(),
            local.registered_at,
        );
        let newest = nudges.last().and_then(|nudge| nudge.event.clone());
        Ok((nudges, newest))
    }

    /// Remember that `session` was shown human plan edits up to `event`.
    pub fn mark_plan_seen(&self, session: &SessionId, event: &EventId) -> Result<()> {
        update_runtime(&self.local, &self.vault, |runtime| {
            if let Some(local) = runtime.sessions.get_mut(session) {
                local.agenda_seen = Some(event.clone());
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Reminders a hook has not shown `session` yet: human plan edits after its
    /// cursor and overdue steps it has not been told about. Returns the
    /// reminders, the plan cursor to store and the keys to remember once the
    /// hook output succeeded (see [`Service::mark_reminded`]).
    pub fn fresh_reminders(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
    ) -> Result<(
        Vec<crate::reduce::nudges::Nudge>,
        Option<EventId>,
        Vec<String>,
    )> {
        // Fresh, not cached: an overdue step is judged against the clock now.
        let (mut reminders, plan_cursor) = self.plan_nudges(project, session, false)?;
        let runtime = self.runtime()?;
        let seen = runtime
            .sessions
            .get(session)
            .map(|local| local.reminded.clone())
            .unwrap_or_default();
        let mut keys = Vec::new();
        for nudge in self.nudges(project, session, false)? {
            let key = reminder_key(&nudge);
            if !seen.contains(&key) {
                keys.push(key);
                reminders.push(nudge);
            }
        }
        Ok((reminders, plan_cursor, keys))
    }

    /// Steps `session` is doing that owe the board an update: past their ETA,
    /// or with no event from the agent for over 15 minutes. Paused steps and
    /// steps waiting on a human are quiet on purpose and never listed.
    pub fn stale_steps(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
    ) -> Result<Vec<(NodePath, String)>> {
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let Some(state) = fresh.state.projects.get(project) else {
            return Ok(Vec::new());
        };
        let silent_for = state
            .sessions
            .get(session)
            .map(|local| (now.millis() - local.last_event_ts.millis()).max(0) / 1000)
            .unwrap_or(0);
        Ok(state
            .features
            .values()
            .filter(|feature| {
                feature.progress == crate::model::Progress::Doing
                    && feature.session.as_ref() == Some(session)
                    && feature.gate == crate::model::Gate::Open
                    && !feature.paused
            })
            .filter_map(|feature| {
                let late = feature
                    .eta
                    .as_ref()
                    .filter(|eta| !eta.paused)
                    .map_or(0, |eta| eta.late_by_s);
                let why = if late > 0 {
                    format!(
                        "{} is {} past its estimate",
                        feature.node,
                        crate::reduce::nudges::short_duration(late)
                    )
                } else if feature.liveness == crate::model::Liveness::Quiet {
                    format!(
                        "{} has had no update for {}",
                        feature.node,
                        crate::reduce::nudges::short_duration(
                            u64::try_from(silent_for).unwrap_or(0)
                        )
                    )
                } else {
                    return None;
                };
                Some((feature.node.clone(), why))
            })
            .collect())
    }

    /// Remember reminders a hook showed `session` (after its output succeeded).
    pub fn mark_reminded(
        &self,
        session: &SessionId,
        keys: &[String],
        plan_cursor: Option<&EventId>,
    ) -> Result<()> {
        if keys.is_empty() && plan_cursor.is_none() {
            return Ok(());
        }
        update_runtime(&self.local, &self.vault, |runtime| {
            if let Some(local) = runtime.sessions.get_mut(session) {
                for key in keys {
                    if !local.reminded.contains(key) {
                        local.reminded.push(key.clone());
                    }
                }
                let excess = local
                    .reminded
                    .len()
                    .saturating_sub(crate::runtime::sessions::MAX_REMINDED);
                local.reminded.drain(..excess);
                if let Some(event) = plan_cursor {
                    local.agenda_seen = Some(event.clone());
                }
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Nudges for `session`: derived reminders such as an overdue ETA. Never
    /// delivered or acked; each one disappears once the agent acts on it.
    pub fn nudges(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        cached: bool,
    ) -> Result<Vec<crate::reduce::nudges::Nudge>> {
        let now = SystemClock.now();
        let fresh = self.refresh(now, cached)?;
        Ok(fresh
            .state
            .projects
            .get(project)
            .map(|state| crate::reduce::nudges::for_session(state, session))
            .unwrap_or_default())
    }

    /// `status`.
    pub fn status(&self, cached: bool) -> Result<Value> {
        let now = SystemClock.now();
        let fresh = self.refresh(now, cached)?;
        match query(&fresh.index, Query::Health, now)? {
            QueryResult::Health {
                files,
                pending,
                invalid,
                generation,
                event_set_digest,
            } => Ok(json!({
                "files": files,
                "pending": pending,
                "invalid": invalid,
                "generation": generation,
                "event_set_digest": event_set_digest,
            })),
            _ => Ok(Value::Null),
        }
    }

    /// `index rebuild`.
    pub fn rebuild_index(&self) -> Result<Value> {
        let now = SystemClock.now();
        let index = rebuild(&self.vault, &self.local, now)?;
        let events: i64 = index
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM raw_events WHERE status = 'valid'",
                [],
                |row| row.get(0),
            )
            .map_err(crate::index::index_error)?;
        let pending: i64 = index
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM raw_events WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )
            .map_err(crate::index::index_error)?;
        let diagnostics: i64 = index
            .connection()
            .query_row("SELECT COUNT(*) FROM diagnostics", [], |row| row.get(0))
            .map_err(crate::index::index_error)?;
        let generation = crate::index::meta(index.connection(), "generation")?
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(json!({
            "events": events,
            "pending": pending,
            "diagnostics": diagnostics,
            "generation": generation,
        }))
    }

    /// `state export --as-of`: canonical State only, no local extras.
    pub fn export_state(&self, as_of: UtcTime) -> Result<State> {
        Ok(self.refresh(as_of, false)?.state)
    }

    /// Mark packets delivered after a successful output. The caller must have
    /// written its output first; a failed write must not call this.
    pub fn mark_delivered(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        packets: &[InboxPacket],
        outcome: DeliveredOutcome,
    ) -> Result<Vec<EventId>> {
        self.mark_delivered_with(project, session, packets, outcome, None)
    }

    /// Mark queue-accepted packets delivered, persisting the durable queue
    /// outcome in the same local attempt record before the Vault event.
    pub fn mark_delivered_with(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        packets: &[InboxPacket],
        outcome: DeliveredOutcome,
        queued: Option<&crate::adapters::QueuedOutcome>,
    ) -> Result<Vec<EventId>> {
        let registration = self.delivery_session(session, project)?;
        let now = SystemClock.now();
        let mut events = Vec::new();
        for packet in packets {
            let (Some(ask), Some(answer)) = (packet.ask.clone(), packet.answer.clone()) else {
                continue;
            };
            if !matches!(packet.kind, PacketKind::Answer | PacketKind::Pivot) {
                continue;
            }
            let key = format!("{}:{}", packet.id.as_str(), session.as_str());
            let reserved = update_runtime(&self.local, &self.vault, |runtime| {
                if let Some(attempt) = runtime.delivery_attempts.get(&key)
                    && attempt.event.is_some()
                {
                    return Ok(None);
                }
                if let Some(attempt) = runtime.delivery_attempts.get(&key)
                    && attempt.reserved.is_some()
                {
                    return Ok(attempt.reserved.clone());
                }
                let reserved = new_event_id()?;
                runtime.delivery_attempts.insert(
                    key.clone(),
                    DeliveryAttempt {
                        packet: packet.id.clone(),
                        session: session.clone(),
                        outcome: outcome_str(outcome).to_owned(),
                        delivered_at: now,
                        event: None,
                        reserved: Some(reserved.clone()),
                        queued: queued.cloned(),
                    },
                );
                Ok(Some(reserved))
            })?;
            let Some(reserved) = reserved.0 else {
                continue;
            };
            let draft = VaultDraft {
                refs: Refs {
                    node: Some(packet.node.clone()),
                    ask: Some(ask),
                    answer: Some(answer),
                    ..Refs::default()
                },
                payload: EventPayload::Delivered(Delivered {
                    session: session.clone(),
                    packet_id: packet.id.clone(),
                    transport: registration.harness,
                    outcome,
                }),
            };
            let appended = self.append(&self.steward_key(project), draft, Some(reserved))?;
            update_runtime(&self.local, &self.vault, |runtime| {
                if let Some(attempt) = runtime.delivery_attempts.get_mut(&key) {
                    attempt.event = Some(appended.event.id.clone());
                    attempt.outcome = outcome_str(outcome).to_owned();
                }
                Ok(())
            })?;
            events.push(appended.event.id);
        }
        Ok(events)
    }

    /// Build the current answer packet for one ask, if a current receipt exists.
    pub fn decision_packet(
        &self,
        fresh: &Fresh,
        project: &ProjectSlug,
        ask_id: &EventId,
    ) -> Result<Option<InboxPacket>> {
        let state = self.project(fresh, project)?;
        let Some(decision) = state.decisions.get(ask_id) else {
            return Err(AgendarError::new(
                ErrorCode::NotFound,
                format!("Decision {} was not found.", ask_id.as_str()),
            )
            .with_detail("decision_not_found")
            .with_hint("Check the ask ID and that its event is visible."));
        };
        if decision.lifecycle == DecisionLifecycle::Withdrawn || decision.superseded_by.is_some() {
            return Err(AgendarError::new(
                ErrorCode::Withdrawn,
                "Decision was withdrawn or superseded.",
            )
            .with_detail("decision_withdrawn")
            .with_hint("Read inbox before acting."));
        }
        let Some(effective) = &decision.effective_receipt else {
            return Ok(None);
        };
        let Some(receipt) = state.receipts.get(effective) else {
            return Ok(None);
        };
        // An override carries the sorted redo set so every affected session
        // knows which features it must restart.
        let redo = crate::reduce::assumptions::pivot_redo(state, &decision.id);
        let kind = if decision.pending_pivot
            || (crate::reduce::assumptions::is_override(state, &decision.id, &state.receipts)
                && !redo.is_empty())
        {
            PacketKind::Pivot
        } else {
            PacketKind::Answer
        };
        Ok(Some(InboxPacket {
            id: InboxPacket::compute_id(kind, project, &decision.node, effective, &redo),
            project: project.clone(),
            node: decision.node.clone(),
            kind,
            ask: Some(decision.id.clone()),
            answer: Some(effective.clone()),
            receipt: Some(receipt.clone()),
            review: None,
            source: effective.clone(),
            redo,
            created_at: receipt.created_at,
        }))
    }

    /// Block until a current answer/default receipt exists, or fail with the
    /// caller timeout. The watcher is subscribed before the first query; only a
    /// changed generation, a reconciliation deadline or the wait deadline causes
    /// another query. Every other wake-up is lease renewal or cancellation.
    /// Wait for one decision answer. Setup, lease extension, every query and
    /// the cleanup all stay inside the caller's absolute deadline when one is
    /// given, so a held runtime lock consumes the budget instead of extending
    /// it.
    pub fn wait_for_answer(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        ask_id: &EventId,
        options: &WaitOptions,
        observer: &mut dyn WaitObserver,
    ) -> Result<InboxPacket> {
        self.require_session(session, project)?;
        if options
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(wait_timeout());
        }
        let watch = VaultWatch::start(&self.vault).ok();
        observer.observed(WaitEvent::Subscribed);
        let relative_deadline = options.timeout.map(|timeout| Instant::now() + timeout);
        let deadline = match (relative_deadline, options.deadline) {
            (Some(relative), Some(absolute)) => Some(relative.min(absolute)),
            (relative, absolute) => relative.or(absolute),
        };
        // Another command may hold the runtime lock for a moment: keep trying
        // until the caller's deadline instead of failing the wait.
        loop {
            match self.extend_lease_until(session, ask_id, options.deadline) {
                Ok(()) => break,
                Err(error)
                    if error.code() == ErrorCode::Busy
                        && options
                            .deadline
                            .is_some_and(|deadline| Instant::now() >= deadline) =>
                {
                    // The budget was spent waiting for the runtime lock; this is
                    // a bounded pending outcome for the caller, not an I/O failure.
                    observer.observed(WaitEvent::TimedOut);
                    return Err(wait_timeout());
                }
                // A budgeted caller (the Stop hook) reports pending instead.
                Err(error) if error.code() == ErrorCode::Busy && options.deadline.is_none() => {
                    if options
                        .cancel
                        .as_ref()
                        .is_some_and(|flag| flag.load(Ordering::SeqCst))
                    {
                        observer.observed(WaitEvent::Cancelled);
                        return Err(cancelled_error());
                    }
                    std::thread::sleep(BUSY_RETRY);
                }
                Err(error) => return Err(error),
            }
        }
        let result = self.wait_loop(project, session, ask_id, options, deadline, watch, observer);
        // Cleanup may also need the runtime lock. Never let it push the caller
        // past its budget; the lease expires on its own TTL instead.
        if options
            .deadline
            .is_none_or(|deadline| Instant::now() < deadline)
        {
            let _ = self.release_lease_until(ask_id, options.deadline);
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn wait_loop(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        ask_id: &EventId,
        options: &WaitOptions,
        deadline: Option<Instant>,
        mut watch: Option<VaultWatch>,
        observer: &mut dyn WaitObserver,
    ) -> Result<InboxPacket> {
        let mut reconcile_due = Instant::now() + options.reconcile;
        let mut lease_due = Instant::now() + options.lease_renew;
        // Only a plain `wait` rides out a busy lock; a budgeted caller (the
        // Stop hook) gets the error and reports pending within its budget.
        let ride_busy = options.deadline.is_none();
        let initial = self.query_wait(
            project,
            ask_id,
            WaitEvent::QueryInitial,
            observer,
            deadline,
            ride_busy,
        )?;
        match initial {
            Looked::Found(packet) => {
                observer.observed(WaitEvent::AnswerFound);
                return Ok(*packet);
            }
            Looked::Busy => reconcile_due = Instant::now() + BUSY_RETRY,
            Looked::NotYet => {}
        }
        loop {
            if options
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
            {
                observer.observed(WaitEvent::Cancelled);
                return Err(cancelled_error());
            }
            let now = Instant::now();
            if deadline.is_some_and(|deadline| now >= deadline) {
                // The final look is still bounded by the same deadline: if the
                // index lock is contended the refresh fails immediately instead
                // of starting an unbounded query after expiry.
                let final_check = self.query_wait(
                    project,
                    ask_id,
                    WaitEvent::QueryDeadline,
                    observer,
                    deadline,
                    ride_busy,
                )?;
                if let Looked::Found(packet) = final_check {
                    observer.observed(WaitEvent::AnswerFound);
                    return Ok(*packet);
                }
                observer.observed(WaitEvent::TimedOut);
                return Err(wait_timeout());
            }
            let mut until = deadline
                .unwrap_or(now + options.reconcile)
                .min(reconcile_due)
                .min(lease_due);
            if until <= now {
                until = now + Duration::from_millis(1);
            }
            // Short bounded OS waits keep cancellation responsive without
            // querying decision state.
            let bounded = until
                .saturating_duration_since(now)
                .min(Duration::from_millis(250));
            let outcome = match &watch {
                Some(watch) => watch.wait(bounded),
                None => {
                    std::thread::sleep(bounded);
                    WaitOutcome::TimedOut
                }
            };
            if options
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::SeqCst))
            {
                observer.observed(WaitEvent::Cancelled);
                return Err(cancelled_error());
            }
            match outcome {
                WaitOutcome::Changed => {
                    match self.query_wait(
                        project,
                        ask_id,
                        WaitEvent::QueryInvalidation,
                        observer,
                        deadline,
                        ride_busy,
                    )? {
                        Looked::Found(packet) => {
                            observer.observed(WaitEvent::AnswerFound);
                            return Ok(*packet);
                        }
                        Looked::Busy => reconcile_due = Instant::now() + BUSY_RETRY,
                        Looked::NotYet => {}
                    }
                }
                WaitOutcome::TimedOut => {}
                WaitOutcome::Closed => {
                    // A dead watcher must fall back to the bounded wait, never
                    // spin on a disconnected channel.
                    watch = None;
                }
            }
            let after = Instant::now();
            if after >= reconcile_due {
                let looked = self.query_wait(
                    project,
                    ask_id,
                    WaitEvent::QueryReconciliation,
                    observer,
                    deadline,
                    ride_busy,
                )?;
                reconcile_due = after + options.reconcile;
                match looked {
                    Looked::Found(packet) => {
                        observer.observed(WaitEvent::AnswerFound);
                        return Ok(*packet);
                    }
                    Looked::Busy => reconcile_due = Instant::now() + BUSY_RETRY,
                    Looked::NotYet => {}
                }
            }
            if after >= lease_due {
                // Lease renewal holds the same runtime lock as acquisition, so
                // it must respect the caller's deadline too.
                match self.extend_lease_until(session, ask_id, deadline) {
                    Ok(()) => {
                        lease_due = after + options.lease_renew;
                        observer.observed(WaitEvent::LeaseRenewed);
                    }
                    // Busy for a moment: renew again shortly; the lease TTL
                    // covers the gap.
                    Err(error) if error.code() == ErrorCode::Busy && ride_busy => {
                        lease_due = Instant::now() + Duration::from_secs(1);
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }

    fn query_wait(
        &self,
        project: &ProjectSlug,
        ask_id: &EventId,
        reason: WaitEvent,
        observer: &mut dyn WaitObserver,
        deadline: Option<Instant>,
        ride_busy: bool,
    ) -> Result<Looked> {
        observer.observed(reason);
        let now = SystemClock.now();
        let fresh = match self.refresh_until(now, false, deadline) {
            Ok(fresh) => fresh,
            // Another command holds the index for a moment: nothing read this
            // time; the loop looks again shortly. Past the deadline it is the
            // caller's bounded outcome, as before.
            Err(error)
                if ride_busy
                    && error.code() == ErrorCode::Busy
                    && deadline.is_none_or(|deadline| Instant::now() < deadline) =>
            {
                return Ok(Looked::Busy);
            }
            Err(error) => return Err(error),
        };
        match self.decision_packet(&fresh, project, ask_id) {
            Ok(Some(packet)) => Ok(Looked::Found(Box::new(packet))),
            Ok(None) => Ok(Looked::NotYet),
            Err(error) if error.code() == ErrorCode::Withdrawn => {
                observer.observed(WaitEvent::Withdrawn);
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    fn extend_lease_until(
        &self,
        session: &SessionId,
        ask_id: &EventId,
        deadline: Option<Instant>,
    ) -> Result<()> {
        let now = SystemClock.now();
        update_runtime_until(&self.local, &self.vault, deadline, |runtime| {
            runtime.wait_leases.insert(
                ask_id.as_str().to_owned(),
                WaitLease {
                    session: session.clone(),
                    ask: ask_id.clone(),
                    updated_at: now,
                    ttl_s: crate::runtime::sessions::WAIT_LEASE_TTL_S,
                },
            );
            Ok(())
        })?;
        Ok(())
    }

    fn release_lease_until(&self, ask_id: &EventId, deadline: Option<Instant>) -> Result<()> {
        update_runtime_until(&self.local, &self.vault, deadline, |runtime| {
            runtime.wait_leases.remove(ask_id.as_str());
            Ok(())
        })?;
        Ok(())
    }

    /// `ack <answer-id>`.
    pub fn ack(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        answer_id: &EventId,
        restarted: Vec<NodePath>,
        request_id: Option<&str>,
    ) -> Result<Value> {
        let registration = self.require_session(session, project)?;
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let decision = state
            .decisions
            .values()
            .find(|decision| decision.answer_history.contains(answer_id))
            .ok_or_else(|| {
                AgendarError::new(
                    ErrorCode::NotFound,
                    format!("Answer {} was not found.", answer_id.as_str()),
                )
                .with_detail("answer_not_found")
                .with_hint("Read inbox and acknowledge the current answer id.")
            })?;
        if decision.lifecycle == DecisionLifecycle::Withdrawn || decision.superseded_by.is_some() {
            return Err(AgendarError::new(
                ErrorCode::Withdrawn,
                "Decision was withdrawn or superseded.",
            )
            .with_detail("decision_withdrawn")
            .with_hint("Read inbox before acting."));
        }
        if decision.effective_receipt.as_ref() != Some(answer_id) {
            return Err(
                AgendarError::new(ErrorCode::StaleAnswer, "Answer is no longer current.")
                    .with_detail("stale_answer")
                    .with_hint("Read inbox before acting."),
            );
        }
        let receipt = state.receipts.get(answer_id).ok_or_else(|| {
            AgendarError::new(ErrorCode::NotFound, "Answer receipt was not found.")
                .with_detail("receipt_not_found")
        })?;
        if receipt.source == crate::model::ReceiptSource::Assumed {
            return Err(AgendarError::validation_at(
                "/answer",
                "assumption_not_ackable",
                "An assumed receipt is applied without an ack; wait for the human answer.",
            ));
        }
        let draft = VaultDraft {
            refs: Refs {
                node: Some(decision.node.clone()),
                ask: Some(decision.id.clone()),
                answer: Some(answer_id.clone()),
                ..Refs::default()
            },
            payload: EventPayload::DecisionAck(DecisionAck {
                agent: Agent {
                    session: session.clone(),
                    harness: registration.harness,
                },
                applied_option: receipt.chose.id.clone(),
                restarted: restarted.clone(),
            }),
        };
        let input = json!({"answer": answer_id, "restarted": restarted});
        self.run_operation(
            "ack",
            project,
            session,
            WriterId::new(self.host.host.clone(), WriterRole::Agent(session.clone())),
            draft,
            request_id,
            &input,
            |appended| {
                let after = self.refresh(SystemClock.now(), false)?;
                let lifecycle = after
                    .state
                    .projects
                    .get(project)
                    .and_then(|state| state.decisions.get(&decision.id))
                    .map(|state| state.lifecycle.as_str())
                    .unwrap_or("acked");
                Ok(json!({
                    "event_id": appended.event.id,
                    "ask_id": decision.id,
                    "answer_id": answer_id,
                    "lifecycle": lifecycle,
                }))
            },
        )
    }

    /// A human answer. This is the shared operator action the app will expose in
    /// a later version; the agent CLI never calls it. It performs no execution.
    pub fn answer_decision(
        &self,
        project: &ProjectSlug,
        ask_id: &EventId,
        option: &crate::model::OptionId,
        done_when: Option<Vec<crate::model::Criterion>>,
        expected_eta: Option<crate::model::EtaRange>,
        note: Option<String>,
    ) -> Result<EventId> {
        self.answer_decision_with(project, ask_id, option, done_when, expected_eta, note, None)
    }

    /// A human answer in their own words (option `own`), when no option fits.
    pub fn answer_own(
        &self,
        project: &ProjectSlug,
        ask_id: &EventId,
        text: String,
        done_when: Option<Vec<crate::model::Criterion>>,
        expected_eta: Option<crate::model::EtaRange>,
    ) -> Result<EventId> {
        let own = crate::model::OptionId::parse(crate::model::cards::OWN_OPTION)?;
        self.answer_decision_with(
            project,
            ask_id,
            &own,
            done_when,
            expected_eta,
            None,
            Some(text),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn answer_decision_with(
        &self,
        project: &ProjectSlug,
        ask_id: &EventId,
        option: &crate::model::OptionId,
        done_when: Option<Vec<crate::model::Criterion>>,
        expected_eta: Option<crate::model::EtaRange>,
        note: Option<String>,
        own_answer: Option<String>,
    ) -> Result<EventId> {
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let decision = state.decisions.get(ask_id).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::NotFound,
                format!("Decision {} was not found.", ask_id.as_str()),
            )
            .with_detail("decision_not_found")
        })?;
        if decision.lifecycle == DecisionLifecycle::Withdrawn || decision.superseded_by.is_some() {
            return Err(AgendarError::new(
                ErrorCode::Withdrawn,
                "Decision was withdrawn or superseded.",
            )
            .with_detail("decision_withdrawn"));
        }
        let own_text = own_answer
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty());
        let chosen = if option.as_str() == crate::model::cards::OWN_OPTION {
            let text = own_text.ok_or_else(|| {
                AgendarError::validation_at(
                    "/own_answer",
                    "own_answer_missing",
                    "An answer in your own words needs its text.",
                )
            })?;
            decision.card.own_option(text)
        } else {
            decision
                .card
                .options
                .iter()
                .find(|candidate| &candidate.id == option)
                .cloned()
                .ok_or_else(|| {
                    AgendarError::validation_at(
                        "/option",
                        "unknown_option",
                        "The chosen option does not belong to this decision.",
                    )
                })?
        };
        let done_when = done_when.unwrap_or_else(|| decision.card.done_when.clone());
        if done_when.is_empty() || done_when.len() > 3 {
            return Err(AgendarError::validation_at(
                "/done_when",
                "done_when_count",
                "done_when must contain 1..=3 criteria.",
            ));
        }
        let expected_eta = expected_eta.or(chosen.eta);
        let mut causes = Vec::new();
        if let Some(ack) = decision.ack_history.last() {
            causes.push(ack.clone());
        }
        if let Some(receipt) = &decision.effective_receipt {
            causes.push(receipt.clone());
        }
        let draft = VaultDraft {
            refs: Refs {
                node: Some(decision.node.clone()),
                ask: Some(ask_id.clone()),
                causes,
                ..Refs::default()
            },
            payload: EventPayload::DecisionAnswer(crate::model::DecisionAnswer {
                option: option.clone(),
                done_when,
                expected_eta,
                note,
                own_answer: (option.as_str() == crate::model::cards::OWN_OPTION)
                    .then(|| own_text.map(str::to_owned))
                    .flatten(),
            }),
        };
        let appended = self.append(&self.operator_key(project), draft, None)?;
        Ok(appended.event.id)
    }

    /// `withdraw <ask-id>`.
    pub fn withdraw(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        ask_id: &EventId,
        reason: String,
        request_id: Option<&str>,
    ) -> Result<Value> {
        let registration = self.require_session(session, project)?;
        let now = SystemClock.now();
        let fresh = self.refresh(now, false)?;
        let state = self.project(&fresh, project)?;
        let decision = state.decisions.get(ask_id).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::NotFound,
                format!("Decision {} was not found.", ask_id.as_str()),
            )
            .with_detail("decision_not_found")
        })?;
        let ask_event = fresh
            .resolution
            .ordered
            .iter()
            .find(|event| &event.id == ask_id)
            .ok_or_else(|| {
                AgendarError::new(
                    ErrorCode::NotFound,
                    format!("Decision {} was not found.", ask_id.as_str()),
                )
                .with_detail("decision_not_found")
            })?;
        let EventPayload::DecisionAsk(body) = &ask_event.payload else {
            return Err(AgendarError::validation_at(
                "/ask",
                "not_an_ask",
                "The event is not a decision ask.",
            ));
        };
        if &body.agent.session != session {
            return Err(AgendarError::validation_at(
                "/ask",
                "withdraw_owner_only",
                "Only the asking agent session may withdraw this decision.",
            ));
        }
        if decision.lifecycle == DecisionLifecycle::Acked {
            return Err(AgendarError::new(
                ErrorCode::Withdrawn,
                "Decision was already acknowledged and cannot be withdrawn.",
            )
            .with_detail("already_acked")
            .with_hint("A later human answer amends an acked decision instead."));
        }
        if decision.lifecycle == DecisionLifecycle::Withdrawn || decision.superseded_by.is_some() {
            return Err(AgendarError::new(
                ErrorCode::Withdrawn,
                "Decision was withdrawn or superseded.",
            )
            .with_detail("decision_withdrawn"));
        }
        let draft = VaultDraft {
            refs: Refs {
                node: Some(decision.node.clone()),
                ask: Some(ask_id.clone()),
                ..Refs::default()
            },
            payload: EventPayload::DecisionWithdrawn(DecisionWithdrawn {
                agent: Agent {
                    session: session.clone(),
                    harness: registration.harness,
                },
                reason: reason.clone(),
            }),
        };
        let input = json!({"ask": ask_id, "reason": reason});
        self.run_operation(
            "withdraw",
            project,
            session,
            WriterId::new(self.host.host.clone(), WriterRole::Agent(session.clone())),
            draft,
            request_id,
            &input,
            |appended| {
                Ok(json!({
                    "event_id": appended.event.id,
                    "ask_id": ask_id,
                    "lifecycle": "withdrawn",
                }))
            },
        )
    }

    /// `report`: publish a report document (already in the Vault as a blob)
    /// with a short summary the app shows. Optionally about one step.
    #[allow(clippy::too_many_arguments)]
    pub fn publish_report(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        node: Option<&NodePath>,
        title: String,
        summary: String,
        report: crate::model::BlobRef,
        request_id: Option<&str>,
    ) -> Result<Value> {
        let registration = self.require_session(session, project)?;
        if let Some(node) = node {
            let now = SystemClock.now();
            let fresh = self.refresh(now, false)?;
            let state = self.project(&fresh, project)?;
            if !state.features.contains_key(node) {
                return Err(AgendarError::new(
                    ErrorCode::NotFound,
                    format!("Feature {} was not found.", node.as_str()),
                )
                .with_detail("feature_not_found"));
            }
        }
        let draft = VaultDraft {
            refs: Refs {
                node: node.cloned(),
                ..Refs::default()
            },
            payload: EventPayload::ReportPublished(crate::model::event::ReportPublished {
                agent: Agent {
                    session: session.clone(),
                    harness: registration.harness,
                },
                title: title.clone(),
                summary: summary.clone(),
                report: report.clone(),
            }),
        };
        let input = json!({"node": node, "title": title, "summary": summary, "report": report});
        self.run_operation(
            "report",
            project,
            session,
            WriterId::new(self.host.host.clone(), WriterRole::Agent(session.clone())),
            draft,
            request_id,
            &input,
            |appended| {
                Ok(json!({
                    "event_id": appended.event.id,
                    "report": report,
                }))
            },
        )
    }

    /// The locally registered session for a harness session id.
    pub fn find_session(
        &self,
        project: &ProjectSlug,
        harness: Harness,
        harness_session_id: &str,
    ) -> Result<Option<SessionId>> {
        let runtime = self.runtime()?;
        Ok(runtime
            .session_for_harness(project, harness, harness_session_id)
            .map(|(id, _)| id))
    }

    /// Packets this local session has not yet been shown.
    pub fn unseen_packets(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
    ) -> Result<Vec<InboxPacket>> {
        let (packets, _, _) = self.inbox(project, session, None, false)?;
        let runtime = self.runtime()?;
        Ok(packets
            .into_iter()
            .filter(|packet| {
                !runtime.delivery_attempts.contains_key(&format!(
                    "{}:{}",
                    packet.id.as_str(),
                    session.as_str()
                ))
            })
            .collect())
    }

    /// The facts the Claude hook shim needs for one session.
    pub fn loop_status(&self, project: &ProjectSlug, session: &SessionId) -> Result<LoopStatus> {
        self.loop_status_until(project, session, None)
    }

    /// The same loop status read, never waiting past `deadline` for the index
    /// locks used by its refresh (the whole Stop-hook budget).
    pub fn loop_status_until(
        &self,
        project: &ProjectSlug,
        session: &SessionId,
        deadline: Option<Instant>,
    ) -> Result<LoopStatus> {
        self.require_session(session, project)?;
        let now = SystemClock.now();
        let fresh = self.refresh_until(now, false, deadline)?;
        let state = self.project(&fresh, project)?;
        let by_id: BTreeMap<EventId, &crate::model::Event> = fresh
            .resolution
            .ordered
            .iter()
            .map(|event| (event.id.clone(), event))
            .collect();
        let mut pending_asks = Vec::new();
        let mut waiting_answers = Vec::new();
        for decision in state.decisions.values() {
            let Some(ask_event) = by_id.get(&decision.id) else {
                continue;
            };
            let EventPayload::DecisionAsk(body) = &ask_event.payload else {
                continue;
            };
            if &body.agent.session != session {
                continue;
            }
            match &decision.effective_receipt {
                None if decision.lifecycle == DecisionLifecycle::Open => {
                    pending_asks.push(decision.id.clone());
                }
                Some(answer)
                    if !decision
                        .applied_sessions
                        .iter()
                        .any(|applied| applied == session) =>
                {
                    let needs_ack = state.receipts.get(answer).is_some_and(|receipt| {
                        receipt.source != crate::model::ReceiptSource::Assumed
                    });
                    waiting_answers.push(WaitingAnswer {
                        ask: decision.id.clone(),
                        answer: answer.clone(),
                        needs_ack,
                    });
                }
                _ => {}
            }
        }
        pending_asks.sort();
        waiting_answers
            .sort_by(|left, right| (&left.ask, &left.answer).cmp(&(&right.ask, &right.answer)));
        let runnable = attention::runnable_nodes(session, state);
        let rules = self.rules(project, false)?;
        let (packets, _, _) = self.inbox(project, session, None, false)?;
        Ok(LoopStatus {
            pending_asks,
            waiting_answers,
            runnable,
            rules,
            packets,
        })
    }

    /// Resolve a project for a harness working directory from local repo links.
    /// The longest linked repo prefix wins; an unmatched directory is a clear
    /// context error that names the link command, never a guess.
    pub fn project_for_cwd(&self, cwd: &Path) -> Result<ProjectSlug> {
        let config = Config::load_or_default(self.local.data_root())?;
        config.project_for_cwd(cwd).ok_or_else(|| {
            AgendarError::new(
                ErrorCode::Context,
                "Missing project for this harness repository.",
            )
            .with_detail("missing_project")
            .with_hint(format!(
                "Run `{} project link --repo <path> --project <slug>` from the repository, or pass --project.",
                shell_quote(&display_path(&self.cli_path))
            ))
        })
    }

    /// A stable platform-quoted command prefix for hook context.
    ///
    /// The first token is the absolute path of the running `agendar` binary: a
    /// harness shell may not have the development tree or a PATH link.
    pub fn command_prefix(&self, project: &ProjectSlug, session: &SessionId) -> String {
        format!(
            "{} --vault {} --data-dir {} --project {} --session {}",
            shell_quote(&display_path(&self.cli_path)),
            shell_quote(&display_path(self.vault.root())),
            shell_quote(&display_path(self.local.data_root())),
            shell_quote(project.as_str()),
            shell_quote(session.as_str()),
        )
    }

    /// The local Codex executable used for queue delivery. This is a local
    /// setting/environment override, never shared Vault state.
    pub fn codex_binary(&self) -> Result<std::path::PathBuf> {
        let config = Config::load_or_default(self.local.data_root())?;
        if let Some(path) = config.codex_bin {
            return Ok(std::path::PathBuf::from(path));
        }
        if let Some(value) = std::env::var_os("AGENDAR_CODEX_BIN") {
            return Ok(std::path::PathBuf::from(value));
        }
        Ok(std::path::PathBuf::from("codex"))
    }
}

/// Every BlobRef a demo points at, without inventing a payload.
fn demo_blobs(demo: &crate::model::Demo) -> Vec<crate::model::BlobRef> {
    use crate::model::Demo;
    match demo {
        Demo::Screenshot(value) => vec![value.blob.clone()],
        Demo::CliTranscript(value) => vec![value.blob.clone()],
        Demo::PreviewUrl(_) => Vec::new(),
        Demo::Recording(value) => vec![value.blob.clone(), value.poster.clone()],
    }
}

fn outcome_str(outcome: DeliveredOutcome) -> &'static str {
    match outcome {
        DeliveredOutcome::Accepted => "accepted",
        DeliveredOutcome::InboxRead => "inbox_read",
    }
}

/// Does a start on this feature repair an already incurred taint?
///
/// A repair is an explicit restart of the tainted feature itself: every other
/// blocker (pause, handoff, an independent open ask, an unresolved upstream)
/// still applies, and only the owner (or an unowned/finished feature) may
/// restart it. The reducer — not this admission check — decides whether the
/// new attempt actually clears the taint, by requiring the current overriding
/// answer in `refs.basis`.
fn repair_restart(session: &SessionId, feature: &crate::model::FeatureState) -> bool {
    feature.basis == crate::model::Basis::Tainted
        && !feature.paused
        && !feature.handoff_pending
        && feature.gate == Gate::Open
        && feature.waits_on.is_empty()
        && (feature.session.is_none()
            || feature.session.as_ref() == Some(session)
            || feature.progress.is_terminal())
}

/// The causal basis a start on `node` must cite.
///
/// It is every unresolved assumption receipt of this feature and of its accepted
/// transitive upstreams, plus the overriding answer that tainted any of them.
/// Citing the override is what repairs that feature's own taint; citing the
/// assumptions is what inherits the remaining speculation.
fn start_basis(state: &ProjectState, node: &NodePath) -> Vec<EventId> {
    let adjacency = attention::adjacency(state);
    let mut relevant: BTreeSet<NodePath> = crate::reduce::graph::ancestors(&adjacency, node)
        .into_iter()
        .collect();
    relevant.insert(node.clone());
    let mut basis: BTreeSet<EventId> = BTreeSet::new();
    for candidate in relevant {
        let Some(feature) = state.features.get(&candidate) else {
            continue;
        };
        basis.extend(feature.assumptions.iter().cloned());
        for taint in &feature.taints {
            if let Some(effective) = state
                .decisions
                .get(taint)
                .and_then(|decision| decision.effective_receipt.clone())
            {
                basis.insert(effective);
            }
        }
    }
    basis.into_iter().collect()
}

fn cancelled_error() -> AgendarError {
    AgendarError::new(ErrorCode::Cancelled, "Cancelled; no agent was stopped.")
        .with_detail("cancelled")
        .with_hint("The pending decision is unaffected.")
}

/// The absolute path used in command instructions.
///
/// `AGENDAR_CLI_PATH` exists for the app bundle and tests; otherwise the
/// running executable is the truth. A bare `agendar` is only a last-resort
/// string when the platform cannot report the current executable.
fn default_cli_path() -> std::path::PathBuf {
    if let Some(value) = std::env::var_os("AGENDAR_CLI_PATH") {
        return std::path::PathBuf::from(value);
    }
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("agendar"))
}

/// A stale mutation precondition: the caller must refresh before acting.
fn stale_token(message: &str, detail: &str) -> AgendarError {
    AgendarError::new(ErrorCode::StaleAnswer, message.to_owned())
        .with_detail(detail)
        .with_hint("Refresh the board and retry the change.")
}

fn char_len(value: &str) -> usize {
    value.chars().count()
}

fn validate_project_patch(patch: &ProjectDeclared) -> Result<()> {
    if patched_project_fields(patch).is_empty() {
        return Err(AgendarError::validation_at(
            "/patch",
            "empty_patch",
            "The patch must contain at least one editable field.",
        ));
    }
    if let Some(title) = &patch.title
        && (char_len(title) == 0 || char_len(title) > 80)
    {
        return Err(AgendarError::validation_at(
            "/patch/title",
            "title_length",
            "A project title must be 1..=80 characters.",
        ));
    }
    for (field, value, max) in [
        ("brief", &patch.brief, 160usize),
        ("goal", &patch.goal, 160usize),
    ] {
        if let Some(value) = value
            && (char_len(value) == 0 || char_len(value) > max)
        {
            return Err(AgendarError::validation_at(
                format!("/patch/{field}"),
                format!("{field}_length"),
                format!("{field} must be 1..={max} characters."),
            ));
        }
    }
    if let Some(glyph) = &patch.glyph
        && (char_len(glyph) == 0 || char_len(glyph) > 24)
    {
        return Err(AgendarError::validation_at(
            "/patch/glyph",
            "glyph_length",
            "glyph must be 1..=24 characters.",
        ));
    }
    Ok(())
}

fn validate_feature_patch(patch: &PlanFeature) -> Result<()> {
    if patched_feature_fields(patch).is_empty() {
        return Err(AgendarError::validation_at(
            "/patch",
            "empty_patch",
            "The patch must contain at least one editable field.",
        ));
    }
    if let Some(title) = &patch.title
        && (char_len(title) == 0 || char_len(title) > 80)
    {
        return Err(AgendarError::validation_at(
            "/patch/title",
            "title_length",
            "A feature title must be 1..=80 characters.",
        ));
    }
    if let Some(priority) = patch.priority
        && priority > 3
    {
        return Err(AgendarError::validation_at(
            "/patch/priority",
            "priority_range",
            "priority must be 0..=3.",
        ));
    }
    if let Some(depends_on) = &patch.depends_on {
        if depends_on.len() > 64 {
            return Err(AgendarError::validation_at(
                "/patch/depends_on",
                "depends_on_limit",
                "depends_on must contain at most 64 node paths.",
            ));
        }
        let mut seen = BTreeSet::new();
        for (index, node) in depends_on.iter().enumerate() {
            if !seen.insert(node.as_str()) {
                return Err(AgendarError::validation_at(
                    format!("/patch/depends_on/{index}"),
                    "duplicate_node",
                    "depends_on must not contain duplicate node paths.",
                ));
            }
        }
    }
    Ok(())
}

fn patched_project_fields(patch: &ProjectDeclared) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if patch.title.is_some() {
        fields.push("title");
    }
    if patch.brief.is_some() {
        fields.push("brief");
    }
    if patch.goal.is_some() {
        fields.push("goal");
    }
    if patch.glyph.is_some() {
        fields.push("glyph");
    }
    if patch.archived.is_some() {
        fields.push("archived");
    }
    fields
}

fn patched_feature_fields(patch: &PlanFeature) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if patch.title.is_some() {
        fields.push("title");
    }
    if patch.slice.is_some() {
        fields.push("slice");
    }
    if patch.demo_kind.is_some() {
        fields.push("demo_kind");
    }
    if patch.milestone.is_some() {
        fields.push("milestone");
    }
    if patch.deadline.is_some() {
        fields.push("deadline");
    }
    if patch.priority.is_some() {
        fields.push("priority");
    }
    if patch.paused.is_some() {
        fields.push("paused");
    }
    if patch.handoff.is_some() {
        fields.push("handoff");
    }
    if patch.depends_on.is_some() {
        fields.push("depends_on");
    }
    fields
}

/// The caller must echo exactly the revisions of the patched fields.
fn check_expected(
    patched: &[&str],
    expected: &BTreeMap<String, Sha256>,
    current: &BTreeMap<String, Sha256>,
) -> Result<()> {
    if expected.len() != patched.len()
        || patched
            .iter()
            .any(|field| !expected.contains_key(*field) || !current.contains_key(*field))
    {
        return Err(AgendarError::validation_at(
            "/expected_fields",
            "expected_fields_mismatch",
            "expected_fields must contain exactly the patched fields.",
        ));
    }
    for field in patched {
        if expected.get(*field) != current.get(*field) {
            return Err(stale_token(
                "An agenda field changed since it was displayed.",
                "stale_field",
            ));
        }
    }
    Ok(())
}

/// The shell-quoted absolute CLI token used in printed instructions.
pub fn cli_token() -> String {
    shell_quote(&default_cli_path().to_string_lossy())
}

/// Quote one value for the platform shell. Values are runtime-local and never
/// committed; the result is directly pasteable into the harness context.
#[cfg(windows)]
pub fn shell_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

/// Quote one value for the platform shell (POSIX single-quote form).
#[cfg(not(windows))]
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// A simple BFS over upstream → downstream edges.
fn reachable(
    adjacency: &BTreeMap<NodePath, BTreeSet<NodePath>>,
    from: &NodePath,
    to: &NodePath,
) -> Option<bool> {
    let mut seen = BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([from.clone()]);
    while let Some(node) = queue.pop_front() {
        if &node == to {
            return Some(true);
        }
        if !seen.insert(node.clone()) {
            continue;
        }
        if let Some(next) = adjacency.get(&node) {
            queue.extend(next.iter().cloned());
        }
    }
    Some(false)
}
