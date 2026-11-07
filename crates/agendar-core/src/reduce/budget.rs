//! Speculation budget projection and admission.
//!
//! Usage is derived from the full converged event set at one `as_of`, never
//! from a snapshot another writer claimed. Defaults: 10 features built on
//! unconfirmed choices, 2 slices awaiting review, 4 h without a demo.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::event::{Budget, BudgetUsage};
use crate::model::ids::{EventId, NodePath, UtcTime};
use crate::model::state::{
    DecisionState, DeliveryState, FeatureState, Progress, Receipt, ReviewStatus,
};
use crate::reduce::assumptions;

/// Counts derived from one converged project state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Activity {
    pub speculative_features: u32,
    pub pending_slices: u32,
    pub demo_gap_s: u64,
}

impl Activity {
    pub fn into_usage(self, budget: &Budget) -> BudgetUsage {
        usage(budget, self)
    }
}

/// Features whose current attempt still carries an unconfirmed assumption.
///
/// A dropped attempt is abandoned work: it never counts against the cap and
/// keeps no live assumption, even though the receipt IDs stay on the attempt
/// for history. The same predicate drives both the projected
/// usage and admission, so a confirmed feature is a new admission again.
pub fn speculative_nodes(
    features: &BTreeMap<NodePath, FeatureState>,
    decisions: &BTreeMap<EventId, DecisionState>,
    receipts: &BTreeMap<EventId, Receipt>,
) -> BTreeSet<NodePath> {
    features
        .iter()
        .filter(|(_, feature)| feature.progress != Progress::Dropped)
        .filter(|(_, feature)| {
            !assumptions::unconfirmed(&feature.assumptions, decisions, receipts).is_empty()
        })
        .map(|(node, _)| node.clone())
        .collect()
}

/// Count of features whose current attempt is still speculative.
pub fn speculative_features(
    features: &BTreeMap<NodePath, FeatureState>,
    decisions: &BTreeMap<EventId, DecisionState>,
    receipts: &BTreeMap<EventId, Receipt>,
) -> u32 {
    speculative_nodes(features, decisions, receipts)
        .len()
        .try_into()
        .unwrap_or(u32::MAX)
}

/// Distinct `(slice, receipt)` pairs whose latest delivery still awaits review.
/// Several demos on one delivery count once, and a superseded delivery never
/// occupies the budget.
pub fn pending_slices(
    deliveries: &BTreeMap<EventId, DeliveryState>,
    features: &BTreeMap<NodePath, FeatureState>,
) -> u32 {
    let mut keys: BTreeSet<(String, EventId)> = BTreeSet::new();
    for delivery in deliveries.values() {
        if delivery.superseded_by.is_some() || delivery.review != ReviewStatus::Pending {
            continue;
        }
        let slice = features
            .get(&delivery.node)
            .map(|feature| feature.slice.clone())
            .unwrap_or_default();
        keys.insert((slice, delivery.receipt.clone()));
    }
    keys.len().try_into().unwrap_or(u32::MAX)
}

/// Wall-clock seconds since the last demo while speculative work is in flight.
///
/// Before the first delivery the earliest active start is the baseline. With no
/// current doing/done-awaiting-review work the gap is zero: an idle project is
/// not "due a demo".
pub fn demo_gap(
    as_of: UtcTime,
    last_demo: Option<UtcTime>,
    earliest_active_start: Option<UtcTime>,
    active: bool,
) -> u64 {
    if !active {
        return 0;
    }
    let base = last_demo.or(earliest_active_start).unwrap_or(as_of);
    u64::try_from((as_of.millis() - base.millis()).max(0) / 1000).unwrap_or(0)
}

fn duration_label(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

/// Project the usage DTO and its blocking reasons.
pub fn usage(budget: &Budget, activity: Activity) -> BudgetUsage {
    let mut reasons = Vec::new();
    if activity.speculative_features > budget.max_speculative_features {
        reasons.push(format!(
            "{} features are built on unconfirmed choices (limit {}).",
            activity.speculative_features, budget.max_speculative_features
        ));
    }
    if activity.pending_slices >= budget.max_pending_slices {
        reasons.push(format!(
            "{} slices await review (limit {}).",
            activity.pending_slices, budget.max_pending_slices
        ));
    }
    let max_gap = u64::from(budget.max_demo_gap_s);
    if activity.demo_gap_s >= max_gap {
        reasons.push(format!(
            "No demo for {} (limit {}).",
            duration_label(activity.demo_gap_s),
            duration_label(max_gap)
        ));
    }
    BudgetUsage {
        speculative_features: activity.speculative_features,
        pending_slices: activity.pending_slices,
        demo_gap_s: activity.demo_gap_s,
        blocked: !reasons.is_empty(),
        reasons,
    }
}

/// Refuse a new speculative admission that the budget cannot absorb.
///
/// `projected_speculative` already includes the feature this operation would
/// introduce. An operation at exactly the cap is admitted only while it does
/// not raise the count; the next unrelated speculative feature fails.
pub fn refuse_new_speculation(
    budget: &Budget,
    activity: Activity,
    projected_speculative: u32,
) -> Option<AgendarError> {
    let mut reasons = Vec::new();
    if projected_speculative > budget.max_speculative_features {
        reasons.push(format!(
            "{} features would be built on unconfirmed choices (limit {}).",
            projected_speculative, budget.max_speculative_features
        ));
    }
    if activity.pending_slices >= budget.max_pending_slices {
        reasons.push(format!(
            "{} slices await review (limit {}).",
            activity.pending_slices, budget.max_pending_slices
        ));
    }
    let max_gap = u64::from(budget.max_demo_gap_s);
    if activity.demo_gap_s >= max_gap {
        reasons.push(format!(
            "No demo for {} (limit {}).",
            duration_label(activity.demo_gap_s),
            duration_label(max_gap)
        ));
    }
    if reasons.is_empty() {
        return None;
    }
    Some(
        AgendarError::new(
            ErrorCode::Budget,
            format!("Speculation budget exceeded: {}", reasons.join(" ")),
        )
        .with_detail("budget_exceeded")
        .with_hint("Ask the human or work on a feature without unconfirmed choices."),
    )
}

/// Admission check for one proposed speculative operation.
pub fn admit_speculation(
    budget: &Budget,
    activity: Activity,
    node: &NodePath,
    speculative: &BTreeSet<NodePath>,
) -> Result<()> {
    let already_speculative = speculative.contains(node);
    let projected = activity
        .speculative_features
        .saturating_add(u32::from(!already_speculative));
    match refuse_new_speculation(budget, activity, projected) {
        None => Ok(()),
        Some(error) => Err(error),
    }
}

/// Whether a feature counts as in-flight work for the demo-gap clock.
pub fn in_flight(feature: &FeatureState, deliveries: &BTreeMap<EventId, DeliveryState>) -> bool {
    let doing = feature.progress == Progress::Doing;
    let awaiting_review = feature.progress == Progress::Done
        && deliveries.values().any(|delivery| {
            delivery.node == feature.node
                && delivery.superseded_by.is_none()
                && delivery.review == ReviewStatus::Pending
        });
    doing || awaiting_review
}
