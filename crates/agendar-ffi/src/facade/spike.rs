//! Early probe calls, kept working beside contract v1. They are not
//! part of the contract.
#![forbid(unsafe_code)]

use agendar_core::index::ingest::{SOURCE_STRAY, SOURCE_WAITING};
use agendar_core::index::query::{Query, QueryResult, query};
use agendar_core::model::{EventId, OptionId};
use agendar_core::vault::{Clock, SystemClock};
use agendar_core::{AgendarError, ErrorCode};

use super::{ColdSplit, FfiError, Session};

/// One project on the home screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSummary {
    pub slug: String,
    pub title: String,
    /// Open items that need the owner.
    pub needs: u32,
    /// Agents currently moving.
    pub moving: u32,
    /// Agents stalled on the owner.
    pub stalled: u32,
    pub last_event: Option<String>,
}

/// One converged read, reduced to what the home screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSummary {
    pub vault_id: String,
    pub evaluated_at: String,
    pub projects: Vec<ProjectSummary>,
}

/// One item that needs the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    pub id: String,
    /// `decide`, `rescue`, `confirm` or `review`.
    pub kind: String,
    pub project: String,
    /// The event the item is about: for `decide`, the ask's event id.
    pub source: String,
    pub node: Option<String>,
    pub question: String,
    pub since: String,
    pub stalled_agents: u32,
}

/// What this host has read of the Vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultHealth {
    pub host: String,
    pub evaluated_at: String,
    pub events: u32,
    pub event_set_digest: Option<String>,
    pub generation: u64,
    pub files: u32,
    /// Writer files the sync app has not downloaded yet.
    pub waiting: Vec<String>,
    /// Files in host folders whose names are not writer files.
    pub strays: Vec<String>,
}

impl Session {
    /// Converge the index and summarize every project that is not archived.
    pub fn snapshot_summary(&self) -> Result<SnapshotSummary, FfiError> {
        let fresh = self.service.refresh(SystemClock.now(), false)?;
        let projects = fresh
            .state
            .projects
            .values()
            .filter(|project| !project.summary.archived)
            .map(|project| {
                let summary = &project.summary;
                ProjectSummary {
                    slug: summary.slug.as_str().to_owned(),
                    title: summary.title.clone(),
                    needs: summary.needs_count,
                    moving: summary.active_agents,
                    stalled: summary.stalled_agents,
                    last_event: summary.last_event.as_ref().map(ToString::to_string),
                }
            })
            .collect();
        Ok(SnapshotSummary {
            vault_id: self.service.vault().id().as_str().to_owned(),
            evaluated_at: fresh.state.evaluated_at.to_string(),
            projects,
        })
    }

    /// Every open item that needs the owner, across projects, in the core's
    /// order per project.
    pub fn needs(&self) -> Result<Vec<Need>, FfiError> {
        let fresh = self.service.refresh(SystemClock.now(), false)?;
        Ok(fresh
            .state
            .projects
            .values()
            .flat_map(|project| project.needs_you.iter())
            .map(|need| Need {
                id: need.id.clone(),
                kind: need.kind.as_str().to_owned(),
                project: need.project.as_str().to_owned(),
                source: need.source.as_str().to_owned(),
                node: need.node.as_ref().map(ToString::to_string),
                question: need.question.clone(),
                since: need.since.to_string(),
                stalled_agents: need.stalled_agents,
            })
            .collect())
    }

    /// Converge the index with the Vault: a full ingest when the index is
    /// empty, a cursor check when nothing changed. Returns the evaluation time.
    /// (Contract v1's `refresh` is in `read.rs`.)
    pub fn spike_refresh(&self) -> Result<String, FfiError> {
        let fresh = self.service.refresh(SystemClock.now(), false)?;
        Ok(fresh.state.evaluated_at.to_string())
    }

    /// Answer the open ask `ask_id` with `option_id`, as this host's operator,
    /// through [`Service::answer_decision`]. Returns the answer's event id.
    ///
    /// The project is the one holding the ask. There is no expected-revision
    /// or surface check yet (`answer_checked` comes with the guard move).
    pub fn answer(&self, ask_id: String, option_id: String) -> Result<String, FfiError> {
        let ask = EventId::parse(&ask_id)?;
        let option = OptionId::parse(&option_id)?;
        let fresh = self.service.refresh(SystemClock.now(), false)?;
        let project = fresh
            .state
            .projects
            .iter()
            .find(|(_, project)| project.decisions.contains_key(&ask))
            .map(|(slug, _)| slug.clone())
            .ok_or_else(|| {
                AgendarError::new(
                    ErrorCode::NotFound,
                    format!("Decision {ask_id} was not found."),
                )
                .with_detail("decision_not_found")
            })?;
        let answer = self
            .service
            .answer_decision(&project, &ask, &option, None, None, None)?;
        Ok(answer.as_str().to_owned())
    }

    /// Converge, then report the local health of this read: the
    /// event count and digest, and the files waiting for the sync app.
    pub fn health(&self) -> Result<VaultHealth, FfiError> {
        let now = SystemClock.now();
        let fresh = self.service.refresh(now, false)?;
        let QueryResult::Health {
            files,
            generation,
            event_set_digest,
            diagnostics,
            ..
        } = query(&fresh.index, Query::Health, now)?
        else {
            return Err(AgendarError::new(
                ErrorCode::Index,
                "The health query returned no health.",
            )
            .into());
        };
        let notes = |code: &str| {
            diagnostics
                .iter()
                .filter(|note| note.code == code)
                .map(|note| note.path.clone())
                .collect()
        };
        Ok(VaultHealth {
            host: self.service.host().as_str().to_owned(),
            evaluated_at: fresh.state.evaluated_at.to_string(),
            events: u32::try_from(fresh.events.len()).unwrap_or(u32::MAX),
            event_set_digest: event_set_digest.map(|digest| digest.as_str().to_owned()),
            generation,
            files: u32::try_from(files.len()).unwrap_or(u32::MAX),
            waiting: notes(SOURCE_WAITING),
            strays: notes(SOURCE_STRAY),
        })
    }

    /// Pure phases of a cold refresh, outside the index.
    pub fn cold_split(&self) -> Result<ColdSplit, FfiError> {
        crate::profile::cold_split(self.service.vault().root())
    }
}
