//! Policy defaults, risk floors, the conservative heuristic scan and the pure
//! `evaluate_policy` / `validate_card` entry points.
//!
//! The locked risk floor and ask-only routing are enforced here; speculation
//! budgets are derived from the policy. The reducer never mutates policy from
//! agent input.

use std::collections::BTreeMap;

use crate::error::{AgendarError, Result};
use crate::model::cards::{Claim, DecisionCard};
use crate::model::event::{Budget, BudgetUsage, PolicyCells, PolicyEvaluation, PolicySet, Preset};
use crate::model::ids::{EventId, Mode, Reach, Reversibility, UtcTime};
use crate::model::state::ProjectState;

/// Locked default budget
pub const DEFAULT_BUDGET: Budget = Budget {
    max_speculative_features: 10,
    max_pending_slices: 2,
    max_demo_gap_s: 14_400,
};

/// Balanced preset cells.
pub const BALANCED_CELLS: PolicyCells = PolicyCells {
    easy_local: Mode::Auto,
    easy_wide: Mode::Assume,
    costly_local: Mode::Assume,
    costly_wide: Mode::Ask,
    irreversible_local: Mode::Ask,
    irreversible_wide: Mode::Ask,
};

pub const STRICT_CELLS: PolicyCells = PolicyCells {
    easy_local: Mode::Ask,
    easy_wide: Mode::Ask,
    costly_local: Mode::Ask,
    costly_wide: Mode::Ask,
    irreversible_local: Mode::Ask,
    irreversible_wide: Mode::Ask,
};

pub const AUTOPILOT_CELLS: PolicyCells = PolicyCells {
    easy_local: Mode::Auto,
    easy_wide: Mode::Auto,
    costly_local: Mode::Assume,
    costly_wide: Mode::Assume,
    irreversible_local: Mode::Ask,
    irreversible_wide: Mode::Ask,
};

/// The initial policy when no eligible `policy.set` exists.
pub fn default_policy() -> PolicySet {
    PolicySet {
        preset: Preset::Balanced,
        cells: BALANCED_CELLS,
        domain_floor: BTreeMap::new(),
        budget: DEFAULT_BUDGET,
        require_human_review: false,
        allow_timeouts: true,
    }
}

/// Preset table lookup: cells and `allow_timeouts` for a named preset.
pub fn preset_values(preset: Preset) -> Option<(PolicyCells, bool)> {
    match preset {
        Preset::Balanced => Some((BALANCED_CELLS, true)),
        Preset::Strict => Some((STRICT_CELLS, false)),
        Preset::Autopilot => Some((AUTOPILOT_CELLS, true)),
        Preset::Custom => None,
    }
}

/// Replicated semantic validation of a `policy.set` body.
///
/// A named preset must match the frozen table exactly (cells and timeout
/// setting); `custom` carries explicit fields. Irreversible cells stay locked to
/// `ask` for every preset, including a custom policy that loosens them.
pub fn validate_policy(value: &PolicySet) -> Result<()> {
    if let Some((cells, allow_timeouts)) = preset_values(value.preset) {
        if value.cells != cells {
            return Err(AgendarError::validation_at(
                "/cells",
                "policy_preset_mismatch",
                "A named policy preset must use its frozen cells.",
            ));
        }
        if value.allow_timeouts != allow_timeouts {
            return Err(AgendarError::validation_at(
                "/allow_timeouts",
                "policy_preset_mismatch",
                "A named policy preset must use its frozen timeout setting.",
            ));
        }
    }
    if value.cells.irreversible_local != Mode::Ask || value.cells.irreversible_wide != Mode::Ask {
        return Err(AgendarError::validation_at(
            "/cells",
            "policy_locked_risk_floor",
            "Irreversible decisions always require ask.",
        ));
    }
    if value.budget.max_speculative_features == 0
        || value.budget.max_speculative_features > 1_000
        || value.budget.max_pending_slices == 0
        || value.budget.max_pending_slices > 100
        || value.budget.max_demo_gap_s < 60
        || value.budget.max_demo_gap_s > 604_800
    {
        return Err(AgendarError::validation_at(
            "/budget",
            "policy_budget_bounds",
            "Speculation budget values are out of bounds.",
        ));
    }
    Ok(())
}

const HEURISTIC_WORDS: [&str; 6] = [
    "deploy",
    "publish",
    "migration",
    "migrations",
    "paid api",
    "paid apis",
];

const HEURISTIC_COMMANDS: [&str; 2] = ["push --force", "rm -rf"];

/// Lowercased, whitespace-normalized scans of the question, why-now and option
/// text. Any match is returned once, in lexical order.
pub fn heuristic_hits(card: &DecisionCard) -> Vec<String> {
    let mut pieces = vec![card.question.clone(), card.why_now.clone()];
    for option in &card.options {
        pieces.push(option.label.clone());
        pieces.push(option.consequence.clone());
    }
    let mut hits = Vec::new();
    for piece in pieces {
        let normalized = normalize_text(&piece);
        for word in HEURISTIC_WORDS {
            if contains_word(&normalized, word) {
                hits.push(word.to_owned());
            }
        }
        for command in HEURISTIC_COMMANDS {
            if normalized.contains(command) {
                hits.push(command.to_owned());
            }
        }
    }
    hits.sort();
    hits.dedup();
    hits
}

fn normalize_text(text: &str) -> String {
    let lowered = text.to_lowercase();
    lowered.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    let starts_with_word = haystack.starts_with(needle)
        && haystack[needle.len()..]
            .chars()
            .next()
            .is_none_or(|next| !is_word_char(next));
    if starts_with_word {
        return true;
    }
    let mut from = 0;
    while let Some(found) = haystack[from..].find(needle) {
        let start = from + found;
        let before = haystack[..start].chars().next_back();
        let after = haystack[start + needle.len()..].chars().next();
        let boundary_before = before.is_none_or(|previous| !is_word_char(previous));
        let boundary_after = after.is_none_or(|next| !is_word_char(next));
        if boundary_before && boundary_after {
            return true;
        }
        from = start + needle.len();
    }
    false
}

fn is_word_char(value: char) -> bool {
    value.is_alphanumeric() || value == '_'
}

/// Worst-case reversibility, external flag and heuristic hits for one card.
pub fn risk_scan(card: &DecisionCard) -> (Reversibility, bool, Vec<String>) {
    let mut risk = card.reversibility;
    let mut external = card.external;
    for option in &card.options {
        if option.reversibility > risk {
            risk = option.reversibility;
        }
        external |= option.external;
    }
    let hits = heuristic_hits(card);
    if !hits.is_empty() {
        risk = Reversibility::Irreversible;
    }
    (risk, external, hits)
}

fn cell_for(policy: &PolicySet, risk: Reversibility, reach: Reach) -> Mode {
    match (risk, reach) {
        (Reversibility::Easy, Reach::Local) => policy.cells.easy_local,
        (Reversibility::Easy, Reach::Wide) => policy.cells.easy_wide,
        (Reversibility::Costly, Reach::Local) => policy.cells.costly_local,
        (Reversibility::Costly, Reach::Wide) => policy.cells.costly_wide,
        (Reversibility::Irreversible, Reach::Local) => policy.cells.irreversible_local,
        (Reversibility::Irreversible, Reach::Wide) => policy.cells.irreversible_wide,
    }
}

/// Evaluate one card against a concrete policy value.
pub fn evaluate_with_policy(
    card: &DecisionCard,
    requested: Mode,
    policy: &PolicySet,
    policy_event: Option<EventId>,
) -> PolicyEvaluation {
    let (risk, external, hits) = risk_scan(card);
    let cell = cell_for(policy, risk, card.reach);
    let floor = policy
        .domain_floor
        .get(&card.domain)
        .copied()
        .unwrap_or(Mode::Auto);
    let hard_floor = if risk == Reversibility::Irreversible || external {
        Mode::Ask
    } else {
        Mode::Auto
    };
    let effective = requested.max(cell).max(floor).max(hard_floor);
    PolicyEvaluation {
        requested,
        effective,
        reversibility: risk,
        external,
        policy_event,
        heuristic_hits: hits,
        budget: BudgetUsage {
            speculative_features: 0,
            pending_slices: 0,
            demo_gap_s: 0,
            demo_gap_node: None,
            blocked: false,
            reasons: Vec::new(),
        },
    }
}

/// Evaluate one card against the project's current policy.
///
/// `policy_event` is the winning `policy.set` event when the caller knows it;
/// the reducer revalidation path supplies the causally observed policy source.
pub fn evaluate_policy_with_event(
    card: &DecisionCard,
    requested: Mode,
    project: &ProjectState,
    policy_event: Option<EventId>,
    _now: UtcTime,
) -> PolicyEvaluation {
    evaluate_with_policy(card, requested, &project.policy.value, policy_event)
}

/// Evaluate one card against the project's current policy, without a policy
/// source event (see [`evaluate_policy_with_event`]).
pub fn evaluate_policy(
    card: &DecisionCard,
    requested: Mode,
    project: &ProjectState,
    now: UtcTime,
) -> PolicyEvaluation {
    evaluate_policy_with_event(card, requested, project, None, now)
}

/// Card validation against derived state.
///
/// Structural limits (options, labels, tables) are owned by the wire decoder.
/// Seen facts need a real observation, so a card that claims one is refused as
/// unimplemented instead of trusting a free-text badge.
pub fn validate_card(card: &DecisionCard, _state: &ProjectState) -> Result<()> {
    if !card
        .options
        .iter()
        .any(|option| option.id == card.recommended.option)
    {
        return Err(AgendarError::validation_at(
            "/recommended/option",
            "unknown_recommendation",
            "The recommended option must be one of the card options.",
        ));
    }
    if card
        .already_checked
        .iter()
        .any(|fact| fact.claim == Claim::Seen)
    {
        return Err(AgendarError::new(
            crate::error::ErrorCode::NotImplemented,
            "Capability `seen_fact_verification` is not implemented in this version.",
        )
        .with_detail("seen_fact_verification")
        .with_hint("Label the fact Agent says until the evidence projection exists."));
    }
    Ok(())
}

/// Speculative starts are refused with `E_NOT_IMPLEMENTED` instead of granting
/// an unsafe free pass; the admission API calls this whenever a proposed start
/// would carry an unconfirmed assumption.
pub fn unsupported_speculation_error() -> AgendarError {
    AgendarError::new(
        crate::error::ErrorCode::NotImplemented,
        "Capability `speculative_start` is not implemented in this version.",
    )
    .with_detail("speculative_start")
    .with_hint("Work on a firm feature until assumptions and budgets exist.")
}

/// Effective weight for cost-of-delay ranking.
pub fn weight_x2(risk: Reversibility, external: bool) -> u8 {
    match risk {
        Reversibility::Irreversible => 4,
        Reversibility::Costly => 3,
        Reversibility::Easy if external => 4,
        Reversibility::Easy => 2,
    }
}
