//! Typed read queries over the projection tables.
//!
//! Queries are bound-parameter SQL only and never interpolate Vault text. Local
//! status, preferences and registration live elsewhere: `snapshot`
//! reports `E_NOT_IMPLEMENTED` until the app shell supplies those local facts.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::index::{Index, index_error, index_error_message, meta};
use crate::model::{
    DecisionState, Event, EventId, EventPayload, InboxPacket, NeedsYouItem, NodePath, PacketKind,
    Progress, ProjectSlug, ProjectState, ProjectSummary, Receipt, ReviewStatus, ReviewVerdict,
    RuleState, SessionId, Sha256, UtcTime, WorkStatus,
};
use crate::reduce::{ReduceContext, reduce};

/// One typed query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Query {
    Snapshot,
    Project {
        project: ProjectSlug,
    },
    Needs {
        project: Option<ProjectSlug>,
        include_snoozed: bool,
    },
    Inbox {
        project: ProjectSlug,
        session: SessionId,
        node: Option<NodePath>,
    },
    Decision {
        ask: EventId,
    },
    Rules {
        project: ProjectSlug,
    },
    History {
        project: ProjectSlug,
        after: Option<EventId>,
        #[serde(default = "default_history_limit")]
        limit: u16,
    },
    Search {
        text: String,
    },
    Health,
}

/// Default page size when `limit` is omitted from a history request.
fn default_history_limit() -> u16 {
    HISTORY_DEFAULT_LIMIT
}

/// One plain-text event summary row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSummary {
    pub id: EventId,
    pub kind: String,
    pub ts: UtcTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<NodePath>,
    pub text: String,
}

/// One source file health row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileHealth {
    pub path: String,
    pub offset: u64,
    pub observed_size: u64,
    pub health: String,
}

/// A local health note about one Vault file: its content is not on this
/// computer yet (`source_waiting`, retryable) or its name is not a writer file
/// (`source_stray`, a conflict copy or foreign file whose events are not read).
/// One more note, `awaiting_newer_version` with path `projects`, counts the
/// event lines this version cannot read but a newer one may.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDiagnostic {
    pub code: String,
    pub message: String,
    pub path: String,
    pub retryable: bool,
}

/// The source notes the last full ingest recorded.
pub fn source_diagnostics(index: &Index) -> Result<Vec<SourceDiagnostic>> {
    use crate::index::ingest::{SOURCE_STRAY, SOURCE_WAITING};
    let mut statement = index
        .connection
        .prepare(
            "SELECT code, message, path FROM diagnostics
             WHERE code IN (?1, ?2) AND path IS NOT NULL ORDER BY path, code",
        )
        .map_err(index_error)?;
    let rows = statement
        .query_map(params![SOURCE_WAITING, SOURCE_STRAY], |row| {
            let code: String = row.get(0)?;
            Ok(SourceDiagnostic {
                retryable: code == SOURCE_WAITING,
                code,
                message: row.get(1)?,
                path: row.get(2)?,
            })
        })
        .map_err(index_error)?;
    let mut notes: Vec<SourceDiagnostic> = rows
        .collect::<std::result::Result<_, _>>()
        .map_err(index_error)?;
    let waiting: i64 = index
        .connection
        .query_row(
            "SELECT COUNT(*) FROM raw_events WHERE reason = ?1",
            params![crate::index::UNDECODABLE],
            |row| row.get(0),
        )
        .map_err(index_error)?;
    if waiting > 0 {
        notes.push(awaiting_newer_version(u64::try_from(waiting).unwrap_or(0)));
    }
    Ok(notes)
}

/// Code of the one health note that counts the lines kept for a newer
/// Agendar version.
pub const AWAITING_NEWER_VERSION: &str = "awaiting_newer_version";

fn awaiting_newer_version(lines: u64) -> SourceDiagnostic {
    let message = if lines == 1 {
        "1 event line was written by a newer Agendar version. It is kept and read automatically once Agendar is updated.".to_owned()
    } else {
        format!(
            "{lines} event lines were written by a newer Agendar version. They are kept and read automatically once Agendar is updated."
        )
    };
    SourceDiagnostic {
        code: AWAITING_NEWER_VERSION.to_owned(),
        message,
        path: "projects".to_owned(),
        retryable: false,
    }
}

/// One rule with the identity of its winning event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleRecord {
    pub rule: crate::model::RuleSet,
    pub event_id: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueryResult {
    Project {
        state: ProjectState,
        graph: crate::model::Graph,
    },
    Needs {
        items: Vec<NeedsYouItem>,
    },
    Inbox {
        packets: Vec<InboxPacket>,
    },
    Decision {
        state: DecisionState,
        receipts: Vec<Receipt>,
    },
    Rules {
        rules: Vec<RuleRecord>,
    },
    History {
        events: Vec<EventSummary>,
    },
    Search {
        needs: Vec<NeedsYouItem>,
        projects: Vec<ProjectSummary>,
    },
    Health {
        files: Vec<FileHealth>,
        pending: u64,
        invalid: u64,
        generation: u64,
        event_set_digest: Option<Sha256>,
        /// Files waiting for the sync app, and stray file names.
        #[serde(default)]
        diagnostics: Vec<SourceDiagnostic>,
    },
}

/// Maximum scalar length of a search string.
pub const SEARCH_MAX_SCALARS: usize = 200;

/// Default history page size.
pub const HISTORY_DEFAULT_LIMIT: u16 = 50;

/// Execute one typed query at one evaluation time.
pub fn query(index: &Index, request: Query, now: UtcTime) -> Result<QueryResult> {
    // Composite queries run in one read transaction: another process committing
    // an ingest can never expose a summary with children from a different
    // generation, or a decision without its matching receipt.
    read_transaction(&index.connection, || query_inner(index, request, now))
}

pub(crate) fn read_transaction<T>(
    connection: &rusqlite::Connection,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    connection
        .execute_batch("BEGIN DEFERRED")
        .map_err(index_error)?;
    match action() {
        Ok(value) => {
            connection.execute_batch("COMMIT").map_err(index_error)?;
            Ok(value)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn query_inner(index: &Index, request: Query, now: UtcTime) -> Result<QueryResult> {
    match request {
        Query::Snapshot => Err(not_implemented("snapshot")),
        Query::Project { project } => {
            let state = load_project(index, &project)?
                .ok_or_else(|| not_found("Project", project.as_str()))?;
            let graph = crate::reduce::graph::build_graph(&state);
            Ok(QueryResult::Project { state, graph })
        }
        Query::Needs {
            project,
            include_snoozed: _,
        } => {
            let mut items = Vec::new();
            match project {
                Some(project) => {
                    if load_project(index, &project)?.is_none() {
                        return Err(not_found("Project", project.as_str()));
                    }
                    items.extend(needs_for_project(index, &project)?);
                }
                None => {
                    for slug in list_projects(index)? {
                        items.extend(needs_for_project(index, &slug)?);
                    }
                }
            }
            sort_needs(&mut items);
            Ok(QueryResult::Needs { items })
        }
        Query::Inbox {
            project,
            session,
            node,
        } => {
            let packets = inbox(index, &project, &session, node.as_ref(), now)?;
            Ok(QueryResult::Inbox { packets })
        }
        Query::Decision { ask } => {
            let state =
                load_decision(index, &ask)?.ok_or_else(|| not_found("Decision", ask.as_str()))?;
            let mut statement = index
                .connection
                .prepare("SELECT json FROM receipts WHERE ask_id = ?1 ORDER BY id")
                .map_err(index_error)?;
            let rows = statement
                .query_map(params![ask.as_str()], |row| row.get::<_, String>(0))
                .map_err(index_error)?;
            let mut receipts = Vec::new();
            for row in rows {
                let json = row.map_err(index_error)?;
                receipts.push(decode(&json)?);
            }
            Ok(QueryResult::Decision { state, receipts })
        }
        Query::Rules { project } => {
            if load_project(index, &project)?.is_none() {
                return Err(not_found("Project", project.as_str()));
            }
            let rules = rules_for_project(index, &project)?;
            Ok(QueryResult::Rules { rules })
        }
        Query::History {
            project,
            after,
            limit,
        } => {
            if load_project(index, &project)?.is_none() {
                return Err(not_found("Project", project.as_str()));
            }
            if limit > 200 {
                return Err(AgendarError::validation_at(
                    "/limit",
                    "history_limit",
                    "History limit must be between 1 and 200.",
                ));
            }
            let events = history(index, &project, after.as_ref(), limit, now)?;
            Ok(QueryResult::History { events })
        }
        Query::Search { text } => {
            if text.trim().is_empty() {
                return Err(AgendarError::validation_at(
                    "/text",
                    "search_text",
                    "Search text must not be blank.",
                ));
            }
            if text.chars().count() > SEARCH_MAX_SCALARS {
                return Err(AgendarError::validation_at(
                    "/text",
                    "search_text_length",
                    "Search text must be at most 200 scalars.",
                ));
            }
            let needle = text.to_lowercase();
            let mut needs = Vec::new();
            let mut all_needs: Vec<NeedsYouItem> = Vec::new();
            let mut projects = Vec::new();
            for slug in list_projects(index)? {
                let Some(state) = load_project(index, &slug)? else {
                    continue;
                };
                let project_matches = [
                    state.summary.slug.as_str(),
                    state.summary.title.as_str(),
                    state.summary.brief.as_str(),
                    state.summary.goal.as_str(),
                ]
                .iter()
                .any(|field| field.to_lowercase().contains(&needle))
                    || state.features.values().any(|feature| {
                        feature.title.to_lowercase().contains(&needle)
                            || feature.node.as_str().to_lowercase().contains(&needle)
                    });
                if project_matches {
                    projects.push(state.summary.clone());
                }
                for row in &state.needs_you {
                    // The tie-break uses every project need, not only the rows
                    // that matched the search text.
                    all_needs.push(row.clone());
                    if row.question.to_lowercase().contains(&needle)
                        || row.reason.to_lowercase().contains(&needle)
                    {
                        needs.push(row.clone());
                    }
                }
            }
            sort_needs(&mut needs);
            let earliest = crate::reduce::ordering::earliest_needs(&all_needs);
            projects.sort_by_key(|summary| {
                crate::reduce::ordering::project_key(summary, earliest.get(&summary.slug).copied())
            });
            Ok(QueryResult::Search { needs, projects })
        }
        Query::Health => {
            let mut statement = index
                .connection
                .prepare(
                    "SELECT path, offset, observed_size, health FROM source_files ORDER BY path",
                )
                .map_err(index_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok(FileHealth {
                        path: row.get(0)?,
                        offset: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                        observed_size: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                        health: row.get(3)?,
                    })
                })
                .map_err(index_error)?;
            let files: Vec<FileHealth> = rows
                .collect::<std::result::Result<_, _>>()
                .map_err(index_error)?;
            let pending: i64 = index
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM raw_events WHERE status = 'pending'",
                    [],
                    |row| row.get(0),
                )
                .map_err(index_error)?;
            let invalid: i64 = index
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM raw_events WHERE status = 'quarantined'",
                    [],
                    |row| row.get(0),
                )
                .map_err(index_error)?;
            let generation = meta(&index.connection, "generation")?
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            let digest = meta(&index.connection, "event_set_digest")?
                .and_then(|value| Sha256::parse(&value).ok());
            Ok(QueryResult::Health {
                diagnostics: source_diagnostics(index)?,
                files,
                pending: u64::try_from(pending).unwrap_or(0),
                invalid: u64::try_from(invalid).unwrap_or(0),
                generation,
                event_set_digest: digest,
            })
        }
    }
}

fn not_found(entity: &str, id: &str) -> AgendarError {
    AgendarError::new(ErrorCode::NotFound, format!("{entity} {id} was not found."))
}

fn not_implemented(capability: &str) -> AgendarError {
    AgendarError::new(
        ErrorCode::NotImplemented,
        format!("Capability {capability} is not implemented in this version."),
    )
    .with_detail(format!("{capability}_not_implemented"))
}

fn decode<T: serde::de::DeserializeOwned>(json: &str) -> Result<T> {
    serde_json::from_str(json).map_err(|_| {
        index_error_message(
            "The local index contains an unreadable projection.",
            "projection_decode",
        )
    })
}

fn list_projects(index: &Index) -> Result<Vec<ProjectSlug>> {
    let mut statement = index
        .connection
        .prepare("SELECT slug FROM projects ORDER BY slug")
        .map_err(index_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(index_error)?;
    let mut projects = Vec::new();
    for row in rows {
        let slug = row.map_err(index_error)?;
        projects
            .push(ProjectSlug::parse(&slug).map_err(|_| {
                index_error_message("Invalid project slug in index.", "index_slug")
            })?);
    }
    Ok(projects)
}

fn load_project(index: &Index, slug: &ProjectSlug) -> Result<Option<ProjectState>> {
    use rusqlite::OptionalExtension as _;
    let summary: Option<String> = index
        .connection
        .query_row(
            "SELECT json FROM projects WHERE slug = ?1",
            params![slug.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(index_error)?;
    let Some(summary) = summary else {
        return Ok(None);
    };
    let summary: ProjectSummary = decode(&summary)?;
    let mut features = BTreeMap::new();
    let mut statement = index
        .connection
        .prepare("SELECT json FROM features WHERE project = ?1 ORDER BY node")
        .map_err(index_error)?;
    let rows = statement
        .query_map(params![slug.as_str()], |row| row.get::<_, String>(0))
        .map_err(index_error)?;
    for row in rows {
        let json = row.map_err(index_error)?;
        let feature: crate::model::FeatureState = decode(&json)?;
        features.insert(feature.node.clone(), feature);
    }
    let mut decisions = BTreeMap::new();
    let mut statement = index
        .connection
        .prepare("SELECT json FROM decisions WHERE project = ?1 ORDER BY ask_id")
        .map_err(index_error)?;
    let rows = statement
        .query_map(params![slug.as_str()], |row| row.get::<_, String>(0))
        .map_err(index_error)?;
    for row in rows {
        let json = row.map_err(index_error)?;
        let decision: DecisionState = decode(&json)?;
        decisions.insert(decision.id.clone(), decision);
    }
    let mut receipts = BTreeMap::new();
    let mut statement = index
        .connection
        .prepare("SELECT json FROM receipts WHERE project = ?1 ORDER BY id")
        .map_err(index_error)?;
    let rows = statement
        .query_map(params![slug.as_str()], |row| row.get::<_, String>(0))
        .map_err(index_error)?;
    for row in rows {
        let json = row.map_err(index_error)?;
        let receipt: Receipt = decode(&json)?;
        receipts.insert(receipt.id.clone(), receipt);
    }
    let mut deliveries = BTreeMap::new();
    let mut statement = index
        .connection
        .prepare("SELECT json FROM deliveries WHERE project = ?1 ORDER BY id")
        .map_err(index_error)?;
    let rows = statement
        .query_map(params![slug.as_str()], |row| row.get::<_, String>(0))
        .map_err(index_error)?;
    for row in rows {
        let json = row.map_err(index_error)?;
        let delivery: crate::model::DeliveryState = decode(&json)?;
        deliveries.insert(delivery.id.clone(), delivery);
    }
    let mut rules: Vec<RuleState> = Vec::new();
    let mut statement = index
        .connection
        .prepare("SELECT json FROM rules WHERE project = ?1 ORDER BY rule_id")
        .map_err(index_error)?;
    let rows = statement
        .query_map(params![slug.as_str()], |row| row.get::<_, String>(0))
        .map_err(index_error)?;
    for row in rows {
        let json = row.map_err(index_error)?;
        rules.push(decode(&json)?);
    }
    let policy_json =
        meta(&index.connection, &format!("policy:{}", slug.as_str()))?.ok_or_else(|| {
            index_error_message(
                "The local index is missing its policy projection.",
                "policy_projection",
            )
        })?;
    let policy = decode(&policy_json)?;
    let mut sessions = BTreeMap::new();
    let mut statement = index
        .connection
        .prepare("SELECT json FROM sessions WHERE project = ?1 ORDER BY session_id")
        .map_err(index_error)?;
    let rows = statement
        .query_map(params![slug.as_str()], |row| row.get::<_, String>(0))
        .map_err(index_error)?;
    for row in rows {
        let json = row.map_err(index_error)?;
        let session: crate::model::SessionState = decode(&json)?;
        sessions.insert(session.id.clone(), session);
    }
    let needs_you = needs_for_project(index, slug)?;
    Ok(Some(ProjectState {
        summary,
        features,
        decisions,
        receipts,
        deliveries,
        rules,
        policy,
        sessions,
        needs_you,
        // Reports live in the reduced state; the index view carries none.
        reports: Vec::new(),
    }))
}

fn needs_for_project(index: &Index, slug: &ProjectSlug) -> Result<Vec<NeedsYouItem>> {
    let mut statement = index
        .connection
        .prepare(
            "SELECT json FROM needs_you WHERE project = ?1
             ORDER BY since, id",
        )
        .map_err(index_error)?;
    let rows = statement
        .query_map(params![slug.as_str()], |row| row.get::<_, String>(0))
        .map_err(index_error)?;
    let mut items = Vec::new();
    for row in rows {
        let json = row.map_err(index_error)?;
        items.push(decode(&json)?);
    }
    Ok(items)
}

/// Needs are ordered by age (`since`, then id) here, never by score; the score
/// fields are still computed and persisted.
fn sort_needs(items: &mut [NeedsYouItem]) {
    crate::reduce::ordering::sort_needs(items);
}

fn rules_for_project(index: &Index, slug: &ProjectSlug) -> Result<Vec<RuleRecord>> {
    let mut statement = index
        .connection
        .prepare("SELECT rule_id, json FROM rules WHERE project = ?1 ORDER BY rule_id")
        .map_err(index_error)?;
    let rows = statement
        .query_map(params![slug.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(index_error)?;
    let mut rules = Vec::new();
    for row in rows {
        let (rule_id, json) = row.map_err(index_error)?;
        let state: RuleState = decode(&json)?;
        if !state.value.enabled {
            continue;
        }
        let source = rule_source(index, slug, &rule_id)?;
        rules.push(RuleRecord {
            rule: state.value,
            event_id: source,
        });
    }
    Ok(rules)
}

/// The winning `rule.set` event for one rule identity.
fn rule_source(index: &Index, slug: &ProjectSlug, rule_id: &str) -> Result<EventId> {
    let (_, resolution) = crate::index::load_resolution(index)?;
    resolution
        .ordered
        .iter()
        .rev()
        .find(|event| {
            &event.project == slug
                && matches!(&event.payload, EventPayload::RuleSet(body) if body.rule_id.as_str() == rule_id)
        })
        .map(|event| event.id.clone())
        .ok_or_else(|| {
            index_error_message(
                "The local index is missing a rule source event.",
                "rule_source",
            )
        })
}

fn load_decision(index: &Index, ask: &EventId) -> Result<Option<DecisionState>> {
    use rusqlite::OptionalExtension as _;
    let json: Option<String> = index
        .connection
        .query_row(
            "SELECT json FROM decisions WHERE ask_id = ?1",
            params![ask.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(index_error)?;
    match json {
        Some(json) => Ok(Some(decode(&json)?)),
        None => Ok(None),
    }
}

fn inbox(
    index: &Index,
    project: &ProjectSlug,
    session: &SessionId,
    node: Option<&NodePath>,
    now: UtcTime,
) -> Result<Vec<InboxPacket>> {
    let (events, resolution) = crate::index::load_resolution(index)?;
    let state = reduce(&events, &ReduceContext { as_of: now });
    let Some(project_state) = state.projects.get(project) else {
        return Err(not_found("Project", project.as_str()));
    };
    let mut packets = review_packets(
        project_state,
        &events,
        &resolution.order_index,
        project,
        session,
        node,
    );
    for decision in project_state.decisions.values() {
        let Some(effective) = &decision.effective_receipt else {
            continue;
        };
        if node.is_some_and(|node| &decision.node != node) {
            continue;
        }
        let Some(receipt) = project_state.receipts.get(effective) else {
            continue;
        };
        // An override turns every `assumed(d)` into redo work: the pivot goes to
        // each affected live session and nowhere else. A repaired feature is no longer tainted, so its
        // session drops out of the recipient set by itself.
        let redo = crate::reduce::assumptions::pivot_redo(project_state, &decision.id);
        let override_pivot = crate::reduce::assumptions::is_override(
            project_state,
            &decision.id,
            &project_state.receipts,
        ) && !redo.is_empty();
        if override_pivot {
            if !crate::reduce::assumptions::pivot_recipients(project_state, &decision.id)
                .iter()
                .any(|recipient| recipient == session)
            {
                continue;
            }
            packets.push(InboxPacket {
                id: InboxPacket::compute_id(
                    PacketKind::Pivot,
                    project,
                    &decision.node,
                    effective,
                    &redo,
                ),
                project: project.clone(),
                node: decision.node.clone(),
                kind: PacketKind::Pivot,
                ask: Some(decision.id.clone()),
                answer: Some(effective.clone()),
                receipt: Some(receipt.clone()),
                review: None,
                delivery: None,
                source: effective.clone(),
                redo,
                created_at: receipt.created_at,
            });
            continue;
        }
        if decision
            .applied_sessions
            .iter()
            .any(|applied| applied == session)
        {
            continue;
        }
        // An ordinary answer is addressed to the step's current session. Until
        // someone applies it any session may pick it up; once applied, it
        // stays only with the step's owner (if that is someone else), never
        // with unrelated sessions.
        let owns_node = project_state
            .features
            .get(&decision.node)
            .and_then(|feature| feature.session.as_ref())
            == Some(session);
        if !decision.pending_pivot && !decision.applied_sessions.is_empty() && !owns_node {
            continue;
        }
        let kind = if decision.pending_pivot {
            PacketKind::Pivot
        } else {
            PacketKind::Answer
        };
        packets.push(InboxPacket {
            id: InboxPacket::compute_id(kind, project, &decision.node, effective, &redo),
            project: project.clone(),
            node: decision.node.clone(),
            kind,
            ask: Some(decision.id.clone()),
            answer: Some(effective.clone()),
            receipt: Some(receipt.clone()),
            review: None,
            delivery: None,
            source: effective.clone(),
            redo,
            created_at: receipt.created_at,
        });
    }
    packets.sort_by(|left, right| {
        (left.created_at.canonical(), left.id.as_str())
            .cmp(&(right.created_at.canonical(), right.id.as_str()))
    });
    Ok(packets)
}

/// Review intents (an accepted or sent-back delivery) for the session that
/// owns the step. A review has no ack: the packet stays until the agent reports
/// progress or delivers again on that step. An accept on a step the agent had
/// already reported finished, or on one that was dropped, is not sent; an
/// accept that finished the step says there is nothing to do.
fn review_packets(
    project_state: &ProjectState,
    events: &[Event],
    order: &BTreeMap<EventId, usize>,
    project: &ProjectSlug,
    session: &SessionId,
    node: Option<&NodePath>,
) -> Vec<InboxPacket> {
    let order_of = |id: &EventId| order.get(id).copied();
    // The causally latest progress or delivery event on each step.
    let mut continued: BTreeMap<&NodePath, usize> = BTreeMap::new();
    for event in events {
        if !matches!(
            event.payload,
            EventPayload::WorkProgress(_) | EventPayload::Delivery(_)
        ) {
            continue;
        }
        let (Some(step), Some(position)) = (event.refs.node.as_ref(), order_of(&event.id)) else {
            continue;
        };
        let latest = continued.entry(step).or_insert(position);
        *latest = (*latest).max(position);
    }
    // Attempts the agent itself reported done or verified.
    let reported_finished: BTreeSet<&EventId> = events
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::WorkProgress(body)
                if matches!(body.status, WorkStatus::Done | WorkStatus::Verified) =>
            {
                event.refs.attempt.as_ref()
            }
            _ => None,
        })
        .collect();
    let mut packets = Vec::new();
    for delivery in project_state.deliveries.values() {
        if delivery.superseded_by.is_some() || delivery.review_conflict {
            continue;
        }
        if node.is_some_and(|node| &delivery.node != node) {
            continue;
        }
        let verdict = match delivery.review {
            ReviewStatus::Pending => continue,
            ReviewStatus::Accepted => ReviewVerdict::Accept,
            ReviewStatus::SentBack => ReviewVerdict::SendBack,
        };
        let Some(review_id) = &delivery.review_event else {
            continue;
        };
        let Some(feature) = project_state.features.get(&delivery.node) else {
            continue;
        };
        if feature.session.as_ref() != Some(session) {
            continue;
        }
        // An accept the step was finished by (a derived done) still reaches the
        // agent once, as "nothing to do"; an accept on work the agent had
        // already reported finished is not sent.
        let finished = matches!(feature.progress, Progress::Done | Progress::Verified)
            && feature
                .attempt
                .as_ref()
                .is_some_and(|attempt| reported_finished.contains(attempt));
        if feature.progress == Progress::Dropped || (finished && verdict == ReviewVerdict::Accept) {
            continue;
        }
        let Some(reviewed) = order_of(review_id) else {
            continue;
        };
        if continued
            .get(&delivery.node)
            .is_some_and(|latest| *latest > reviewed)
        {
            continue;
        }
        let Some(review) = events.iter().find(|event| &event.id == review_id) else {
            continue;
        };
        let EventPayload::ReviewResult(body) = &review.payload else {
            continue;
        };
        packets.push(InboxPacket {
            id: InboxPacket::compute_id(
                PacketKind::Review,
                project,
                &delivery.node,
                review_id,
                &[],
            ),
            project: project.clone(),
            node: delivery.node.clone(),
            kind: PacketKind::Review,
            ask: None,
            answer: None,
            receipt: None,
            review: Some(body.clone()),
            delivery: Some(delivery.id.clone()),
            source: review_id.clone(),
            redo: Vec::new(),
            created_at: review.ts,
        });
    }
    packets
}

fn history(
    index: &Index,
    project: &ProjectSlug,
    after: Option<&EventId>,
    limit: u16,
    now: UtcTime,
) -> Result<Vec<EventSummary>> {
    let limit = usize::from(limit.clamp(1, 200));
    let (_, resolution) = crate::index::load_resolution(index)?;
    let mut ordered: Vec<&crate::model::Event> = resolution
        .ordered
        .iter()
        .filter(|event| &event.project == project)
        // A snooze is the owner's display state, not project history.
        .filter(|event| !crate::reduce::snooze::is_snooze(event))
        .collect();
    if let Some(after) = after {
        let index = ordered
            .iter()
            .position(|event| &event.id == after)
            .ok_or_else(|| not_found("Event", after.as_str()))?;
        ordered = ordered.split_off(index + 1);
    }
    let _ = now;
    Ok(ordered.into_iter().take(limit).map(summarize).collect())
}

/// The newest `limit` valid events of one project, returned oldest first.
///
/// A read-only local query for the non-canonical activity feed: it reads
/// `raw_events` rows whose status is `valid` and never touches the reducer or
/// the projection tables. Rows that no longer decode are skipped.
pub fn recent_project_events(
    index: &Index,
    project: &ProjectSlug,
    limit: usize,
) -> Result<Vec<crate::model::Event>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut statement = index
        .connection
        .prepare(
            "SELECT id, digest, json, writer FROM raw_events
             WHERE project = ?1 AND status = 'valid' AND kind != 'need.snoozed'
             ORDER BY ts DESC, id DESC
             LIMIT ?2",
        )
        .map_err(index_error)?;
    let rows = statement
        .query_map(params![project.as_str(), limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(index_error)?;
    let mut events = Vec::new();
    for row in rows {
        let (id, digest, json, writer) = row.map_err(index_error)?;
        let (Ok(id), Ok(digest)) = (EventId::parse(&id), Sha256::parse(&digest)) else {
            continue;
        };
        if let Some(event) = crate::index::decode_stored(&id, &digest, &json, &writer) {
            events.push(event);
        }
    }
    events.reverse();
    Ok(events)
}

fn summarize(event: &crate::model::Event) -> EventSummary {
    EventSummary {
        id: event.id.clone(),
        kind: event.kind().to_owned(),
        ts: event.ts,
        node: event.refs.node.clone(),
        text: summary_text(event),
    }
}

fn summary_text(event: &crate::model::Event) -> String {
    match &event.payload {
        EventPayload::ProjectDeclared(body) => format!(
            "Declared project {}",
            body.title.as_deref().unwrap_or("(no title)")
        ),
        EventPayload::PlanFeature(body) => format!(
            "Planned feature {}",
            body.title.as_deref().unwrap_or("(unnamed)")
        ),
        EventPayload::NeedSnoozed(_) => "Snoozed a row".to_owned(),
        EventPayload::PlanDropped(_) => format!(
            "Dropped {} (owner)",
            event
                .refs
                .node
                .as_ref()
                .map_or("a step", crate::model::NodePath::as_str)
        ),
        EventPayload::WorkProgress(body) => format!(
            "Work {} on {}",
            match body.status {
                crate::model::WorkStatus::Started => "started",
                crate::model::WorkStatus::Done => "done",
                crate::model::WorkStatus::Verified => "verified",
                crate::model::WorkStatus::Dropped => "dropped",
            },
            event
                .refs
                .node
                .as_ref()
                .map(NodePath::as_str)
                .unwrap_or("(no node)")
        ),
        EventPayload::Pulse(body) => format!("Pulse: {}", body.message),
        EventPayload::DecisionAsk(body) => format!("Asked: {}", body.card.question),
        EventPayload::DecisionAssumed(body) => {
            format!("Assumed choice {}", body.option.as_str())
        }
        EventPayload::DecisionAuto(body) => format!("Automatic choice {}", body.option.as_str()),
        EventPayload::DecisionWithdrawn(_) => "Withdrew a decision".to_owned(),
        EventPayload::ReportPublished(body) => format!("Published a report: {}", body.title),
        EventPayload::DecisionAnswer(body) if body.own_answer.is_some() => {
            "Answered in their own words".to_owned()
        }
        EventPayload::DecisionAnswer(body) => format!("Answered {}", body.option.as_str()),
        EventPayload::DecisionAck(body) => {
            format!("Acknowledged {}", body.applied_option.as_str())
        }
        EventPayload::Delivery(body) => format!("Delivered {}", body.name),
        EventPayload::EvidenceObserved(body) => {
            format!("Observed criterion {}", body.criterion.as_str())
        }
        EventPayload::ReviewResult(body) => format!(
            "Review {}",
            match body.verdict {
                crate::model::cards::ReviewVerdict::Accept => "accepted",
                crate::model::cards::ReviewVerdict::SendBack => "sent back",
            }
        ),
        EventPayload::RuleSet(body) => format!(
            "Rule {} {}",
            body.rule_id.as_str(),
            if body.enabled { "enabled" } else { "disabled" }
        ),
        EventPayload::PolicySet(body) => format!("Policy set to {:?}", body.preset).to_lowercase(),
        EventPayload::SessionLost(_) => "Session lost".to_owned(),
        EventPayload::SessionContinued(_) => "Session continued".to_owned(),
        EventPayload::Delivered(_) => "Packet delivered".to_owned(),
    }
}
