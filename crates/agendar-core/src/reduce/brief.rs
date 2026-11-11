//! The six-section Brief projection.
//!
//! Rows are projections, never events. `since` is the local "last looked"
//! watermark: only deliveries newer than it appear under Delivered, so the
//! section really is "since you last looked". Everything else is current state.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::Msg;
use crate::model::event::Event;
use crate::model::ids::{EventId, NodePath, ProjectSlug, UtcTime};
use crate::model::state::{
    DecisionLifecycle, FeatureState, NeedsKind, NeedsYouItem, Progress, State,
};
use crate::reduce::ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BriefTargetKind {
    Need,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefTarget {
    pub kind: BriefTargetKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefRow {
    pub id: String,
    pub text: String,
    /// `text` as a catalog message when the core wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<Msg>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// `detail` as a catalog message when the core wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_msg: Option<Msg>,
    pub target: BriefTarget,
    pub source_ids: Vec<EventId>,
}

impl BriefRow {
    fn project(id: impl Into<String>, text: impl Into<String>, slug: &ProjectSlug) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            msg: None,
            detail: None,
            detail_msg: None,
            target: BriefTarget {
                kind: BriefTargetKind::Project,
                id: slug.as_str().to_owned(),
            },
            source_ids: Vec::new(),
        }
    }

    fn need(id: impl Into<String>, text: impl Into<String>, need: &NeedsYouItem) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            msg: None,
            detail: None,
            detail_msg: None,
            target: BriefTarget {
                kind: BriefTargetKind::Need,
                id: need.id.clone(),
            },
            source_ids: vec![need.source.clone()],
        }
    }

    fn with_msg(mut self, msg: Msg) -> Self {
        self.msg = Some(msg);
        self
    }

    fn with_detail_msg(mut self, msg: Option<Msg>) -> Self {
        self.detail_msg = msg;
        self
    }

    fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        if !detail.is_empty() {
            self.detail = Some(detail);
        }
        self
    }

    fn with_source(mut self, source: &EventId) -> Self {
        if !self.source_ids.contains(source) {
            self.source_ids.push(source.clone());
        }
        self
    }
}

/// The six sections in their frozen order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefData {
    pub delivered: Vec<BriefRow>,
    pub coming_up: Vec<BriefRow>,
    pub needs_you: Vec<BriefRow>,
    pub if_you_wait: Vec<BriefRow>,
    pub by_default: Vec<BriefRow>,
    pub at_risk: Vec<BriefRow>,
}

impl BriefData {
    pub fn is_empty(&self) -> bool {
        self.delivered.is_empty()
            && self.coming_up.is_empty()
            && self.needs_you.is_empty()
            && self.if_you_wait.is_empty()
            && self.by_default.is_empty()
            && self.at_risk.is_empty()
    }
}

fn event_times(events: &[Event]) -> BTreeMap<EventId, UtcTime> {
    events
        .iter()
        .map(|event| (event.id.clone(), event.ts))
        .collect()
}

fn plural(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {plural}")
    }
}

/// Project every brief row from converged state plus the "last looked" mark.
pub fn build(state: &State, events: &[Event], since: Option<UtcTime>) -> BriefData {
    let times = event_times(events);
    let mut needs: Vec<NeedsYouItem> = state
        .projects
        .values()
        .flat_map(|project| project.needs_you.iter())
        .filter(|need| crate::reduce::attention::is_active_need(need))
        .cloned()
        .collect();
    ordering::sort_needs(&mut needs);
    let earliest = ordering::earliest_needs(&needs);
    let mut slugs: Vec<ProjectSlug> = state.projects.keys().cloned().collect();
    slugs.sort_by_key(|slug| {
        let summary = &state.projects[slug].summary;
        ordering::project_key(summary, earliest.get(slug).copied())
    });

    let mut brief = BriefData::default();

    // Delivered: accepted, non-superseded deliveries after the watermark.
    let mut delivered: Vec<(UtcTime, BriefRow)> = Vec::new();
    for slug in &slugs {
        let project = &state.projects[slug];
        for delivery in project.deliveries.values() {
            if delivery.review != crate::model::state::ReviewStatus::Accepted
                || delivery.superseded_by.is_some()
            {
                continue;
            }
            let Some(ts) = times.get(&delivery.id).copied() else {
                continue;
            };
            if since.is_some_and(|since| ts <= since) {
                continue;
            }
            let mut row = BriefRow::project(
                format!("delivered:{}", delivery.id.as_str()),
                format!("{} accepted", delivery.name),
                slug,
            )
            .with_msg(Msg::new("brief.accepted").text("name", delivery.name.clone()))
            .with_detail(project.summary.title.clone())
            .with_source(&delivery.id);
            if let Some(review) = &delivery.review_event {
                row = row.with_source(review);
            }
            delivered.push((ts, row));
        }
    }
    delivered.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    brief.delivered = delivered.into_iter().map(|(_, row)| row).collect();

    // Coming up: features with a live attempt and its ETA.
    let mut coming: Vec<((usize, i64, String), BriefRow)> = Vec::new();
    for (index, slug) in slugs.iter().enumerate() {
        let project = &state.projects[slug];
        for feature in project.features.values() {
            if feature.progress != Progress::Doing {
                continue;
            }
            let latest = feature
                .eta
                .as_ref()
                .map(|eta| {
                    eta.likely_finish
                        .as_ref()
                        .unwrap_or(&eta.agent_finish)
                        .latest
                })
                .map_or(i64::MAX, |when| when.millis());
            let mut row = BriefRow::project(
                format!("coming_up:{}:{}", slug.as_str(), feature.node.as_str()),
                feature.title.clone(),
                slug,
            )
            .with_detail(project.summary.title.clone());
            if let Some(attempt) = &feature.attempt {
                row = row.with_source(attempt);
            }
            coming.push(((index, latest, feature.node.as_str().to_owned()), row));
        }
    }
    coming.sort_by(|left, right| left.0.cmp(&right.0));
    brief.coming_up = coming.into_iter().map(|(_, row)| row).collect();

    // Needs you: the same ranked rows the sidebar shows.
    brief.needs_you = needs
        .iter()
        .map(|need| {
            let mut row = BriefRow::need(
                format!("needs_you:{}", need.id),
                need.question.clone(),
                need,
            )
            .with_detail(need.reason.clone())
            .with_detail_msg(need.reason_msg.clone());
            row.msg.clone_from(&need.question_msg);
            row
        })
        .collect();

    // If you wait: blocked steps downstream of the top decision.
    if let Some(top) = needs.iter().find(|row| row.kind == NeedsKind::Decide)
        && let Some(node) = &top.node
        && let Some(project) = state.projects.get(&top.project)
        && let Some(feature) = project.features.get(node)
    {
        let blocked = feature
            .unblocks
            .iter()
            .filter(|candidate| project.features.get(*candidate).is_none_or(is_blocked))
            .count();
        if blocked > 0 {
            brief.if_you_wait.push(
                BriefRow::need(
                    format!("if_you_wait:{}", top.id),
                    format!("{} wait on your answer", plural(blocked, "step", "steps")),
                    top,
                )
                .with_msg(Msg::new("brief.wait_on_answer").count("n", blocked))
                .with_detail(top.question.clone())
                .with_detail_msg(top.question_msg.clone()),
            );
        }
    }

    // By default: open human asks that will proceed on their own.
    let mut defaults: Vec<((UtcTime, EventId), BriefRow)> = Vec::new();
    for slug in &slugs {
        let project = &state.projects[slug];
        if !project.policy.value.allow_timeouts {
            continue;
        }
        for decision in project.decisions.values() {
            if decision.lifecycle != DecisionLifecycle::Open
                || decision.mode != crate::model::ids::Mode::Ask
            {
                continue;
            }
            let Some(default_at) = decision.default_at else {
                continue;
            };
            let recommended = decision
                .card
                .options
                .iter()
                .find(|option| option.id == decision.card.recommended.option)
                .map(|option| option.label.clone());
            let need = project
                .needs_you
                .iter()
                .find(|row| row.source == decision.id);
            let row = match need {
                Some(need) => BriefRow::need(
                    format!("by_default:{}", decision.id.as_str()),
                    need.question.clone(),
                    need,
                ),
                None => BriefRow::project(
                    format!("by_default:{}", decision.id.as_str()),
                    decision.card.question.clone(),
                    slug,
                )
                .with_source(&decision.id),
            };
            defaults.push((
                (default_at, decision.id.clone()),
                match recommended {
                    Some(label) => row
                        .with_detail(format!("Then goes with {label}"))
                        .with_detail_msg(Some(Msg::new("brief.then_goes").text("label", label))),
                    None => row
                        .with_detail("Then goes with the recommendation")
                        .with_detail_msg(Some(Msg::new("brief.then_goes_rec"))),
                },
            ));
        }
    }
    defaults.sort_by(|left, right| left.0.cmp(&right.0));
    brief.by_default = defaults.into_iter().map(|(_, row)| row).collect();

    // At risk: budget pressure and late features.
    for slug in &slugs {
        let project = &state.projects[slug];
        let usage = &project.summary.budget;
        let budget = &project.policy.value.budget;
        if usage.blocked {
            brief.at_risk.push(
                BriefRow::project(
                    format!("at_risk:budget:{}", slug.as_str()),
                    format!("{} is over its speculation budget", project.summary.title),
                    slug,
                )
                .with_msg(
                    Msg::new("brief.over_budget").text("title", project.summary.title.clone()),
                )
                .with_detail(usage.reasons.join(" "))
                .with_detail_msg(Some(over_budget_msg(
                    usage,
                    budget,
                    &project.features,
                ))),
            );
        } else if near_limit(usage.speculative_features, budget.max_speculative_features)
            || near_limit(usage.pending_slices, budget.max_pending_slices)
            || near_limit(
                u32::try_from(usage.demo_gap_s).unwrap_or(u32::MAX),
                budget.max_demo_gap_s,
            )
        {
            brief.at_risk.push(
                BriefRow::project(
                    format!("at_risk:near_budget:{}", slug.as_str()),
                    format!(
                        "{} is close to its speculation budget",
                        project.summary.title
                    ),
                    slug,
                )
                .with_msg(
                    Msg::new("brief.near_budget").text("title", project.summary.title.clone()),
                )
                .with_detail(format!(
                    "{} of {} speculative features used",
                    usage.speculative_features, budget.max_speculative_features
                ))
                .with_detail_msg(Some(
                    Msg::new("brief.budget_used")
                        .count("used", usage.speculative_features)
                        .count("limit", budget.max_speculative_features),
                )),
            );
        }
        for feature in project.features.values() {
            let Some(eta) = &feature.eta else {
                continue;
            };
            if eta.late_by_s == 0 {
                continue;
            }
            let mut row = BriefRow::project(
                format!("at_risk:late:{}:{}", slug.as_str(), feature.node.as_str()),
                format!("{} is late", feature.title),
                slug,
            )
            .with_msg(Msg::new("brief.late").text("title", feature.title.clone()))
            .with_detail(project.summary.title.clone());
            if let Some(attempt) = &feature.attempt {
                row = row.with_source(attempt);
            }
            brief.at_risk.push(row);
        }
    }

    brief
}

/// The budget's blocking reasons (`reduce::budget::usage`) as one message.
fn over_budget_msg(
    usage: &crate::model::event::BudgetUsage,
    budget: &crate::model::Budget,
    features: &BTreeMap<NodePath, FeatureState>,
) -> Msg {
    let mut reasons = Vec::new();
    if usage.speculative_features > budget.max_speculative_features {
        reasons.push(
            Msg::new("budget.speculative")
                .count("n", usage.speculative_features)
                .count("limit", budget.max_speculative_features),
        );
    }
    if usage.pending_slices >= budget.max_pending_slices {
        reasons.push(
            Msg::new("budget.slices")
                .count("n", usage.pending_slices)
                .count("limit", budget.max_pending_slices),
        );
    }
    if usage.demo_gap_s >= u64::from(budget.max_demo_gap_s) {
        // Named with the step the clock runs for; plain when none is in progress.
        let step = usage
            .demo_gap_node
            .as_ref()
            .and_then(|node| features.get(node));
        let reason = match step {
            Some(step) => Msg::new("budget.demo_gap").text("step", step.title.clone()),
            None => Msg::new("budget.demo_gap.no_step"),
        };
        reasons.push(
            reason
                .minutes("gap", usage.demo_gap_s / 60)
                .minutes("limit", u64::from(budget.max_demo_gap_s) / 60),
        );
    }
    Msg::new("budget.reasons").list("reasons", reasons)
}

fn is_blocked(feature: &FeatureState) -> bool {
    !matches!(feature.progress, Progress::Verified | Progress::Dropped)
}

/// A count is near its cap at four fifths of the limit (`0` usage never is).
fn near_limit(used: u32, limit: u32) -> bool {
    used > 0 && limit > 0 && u64::from(used) * 5 >= u64::from(limit) * 4
}
