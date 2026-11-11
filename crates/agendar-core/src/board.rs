//! Pages of `agendar board`: one page of open steps, one page per milestone,
//! and the whole board. Each page names the command that fetches the others.

use serde_json::{Value, json};

use crate::error::{AgendarError, Result};
use crate::model::state::Progress;
use crate::model::state::{FeatureState, ProjectState};

/// Which part of the board to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardView {
    /// Open steps of every milestone plus the page index (the default).
    Open,
    /// One milestone; `None` is the steps without a milestone.
    Milestone(Option<String>),
    /// Every step in full (the board before pages existed).
    All,
}

/// Pointer from the default page to the other ways of reading the board.
pub const HINT: &str = "Open steps only. Run a page's `cmd` for one milestone with its finished steps; `board --all` lists every step; add `--out <file>` to save it instead of printing.";

/// Done, verified and dropped steps are finished.
pub const fn finished(progress: Progress) -> bool {
    matches!(
        progress,
        Progress::Done | Progress::Verified | Progress::Dropped
    )
}

/// The exact command for one milestone page. The steps without a milestone
/// are `--milestone ''` (a real milestone is never empty).
pub fn page_cmd(milestone: Option<&str>) -> String {
    let name = milestone.unwrap_or_default();
    format!(
        "agendar board --milestone '{}'",
        name.replace('\'', "'\\''")
    )
}

/// One entry per milestone in plan order (steps without one under null),
/// with its open and finished counts and the command for its page.
pub fn pages(state: &ProjectState) -> Vec<Value> {
    let mut pages: Vec<(Option<&str>, [u64; 4])> = Vec::new();
    for feature in state.features.values() {
        let milestone = feature.milestone.as_deref();
        let index = pages
            .iter()
            .position(|(seen, _)| *seen == milestone)
            .unwrap_or_else(|| {
                pages.push((milestone, [0; 4]));
                pages.len() - 1
            });
        let slot = match feature.progress {
            Progress::Planned | Progress::Doing => 0,
            Progress::Done => 1,
            Progress::Verified => 2,
            Progress::Dropped => 3,
        };
        pages[index].1[slot] += 1;
    }
    pages
        .into_iter()
        .map(|(milestone, [open, done, verified, dropped])| {
            json!({
                "milestone": milestone,
                "open": open,
                "done": done,
                "verified": verified,
                "dropped": dropped,
                "cmd": page_cmd(milestone),
            })
        })
        .collect()
}

/// The steps of one milestone page, or a validation error naming the pages.
pub fn milestone_steps<'a>(
    state: Option<&'a ProjectState>,
    milestone: Option<&str>,
) -> Result<(&'a ProjectState, Vec<&'a FeatureState>)> {
    let steps: Vec<&FeatureState> = state
        .into_iter()
        .flat_map(|state| state.features.values())
        .filter(|feature| feature.milestone.as_deref() == milestone)
        .collect();
    if let (Some(state), false) = (state, steps.is_empty()) {
        return Ok((state, steps));
    }
    let mut names: Vec<String> = Vec::new();
    for feature in state.into_iter().flat_map(|state| state.features.values()) {
        let name = feature.milestone.as_deref().map_or_else(
            || "'' (no milestone)".to_owned(),
            |name| format!("'{name}'"),
        );
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let valid = if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(", ")
    };
    Err(AgendarError::validation_at(
        "/milestone",
        "unknown_milestone",
        format!(
            "No step has milestone '{}'. Milestones: {valid}.",
            milestone.unwrap_or_default()
        ),
    )
    .with_hint("Run `agendar board` for the page index with each page's command."))
}

/// A finished step on a milestone page: enough to know it exists.
pub fn short_step(feature: &FeatureState) -> Value {
    let mut step = json!({
        "node": feature.node,
        "title": feature.title,
        "progress": feature.progress,
    });
    dropped_by(feature, &mut step);
    step
}

/// Say that the owner dropped the step: `"dropped_by": "owner"` and their
/// note. A step an agent dropped has neither.
pub fn dropped_by(feature: &FeatureState, step: &mut Value) {
    let (Some(drop), Some(object)) = (&feature.owner_drop, step.as_object_mut()) else {
        return;
    };
    object.insert("dropped_by".into(), Value::from("owner"));
    if let Some(note) = &drop.note {
        object.insert("drop_note".into(), Value::from(note.clone()));
    }
}
