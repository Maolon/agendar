//! Notification eligibility, routing metadata and the injectable transport.
//!
//! The app owns the registry, authorization tokens and dedupe; this module only
//! decides which items may be announced, what buttons they offer, and hands an
//! owned [`NoticeSpec`] to a [`NoticeSink`]. Tests inject a fake sink here; the
//! native UserNotifications/WinRT/D-Bus backends live in `agendar-notify` and
//! are reached only through [`crate::native::NativeSink`].

use std::collections::{BTreeMap, BTreeSet};

use agendar_core::model::state::Liveness;
use agendar_core::model::{
    DecisionLifecycle, DecisionOption, DecisionState, MsgArg, NeedsKind, NeedsYouItem, OptionId,
    Progress, ProjectState, UtcTime,
};
use agendar_core::reduce::attention::notification_eligible;
use agendar_core::reduce::policy::risk_scan;

use crate::preferences::{Lang, fill};

/// Whether this process can turn a notification action into an app event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Unknown,
    Available,
    Unavailable,
}

impl Availability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Available => "available",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One owned notification request. No Vault path, transcript or token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeSpec {
    /// Random 128-bit routing handle, 32 lowercase hex characters.
    pub id: String,
    pub title: String,
    pub body: String,
    /// Labels of the answer buttons next to Open (easy questions only);
    /// empty offers Open only.
    pub choices: Vec<String>,
    pub project: String,
    pub item_id: String,
}

/// The platform notification transport, injected so tests never touch the OS.
pub trait NoticeSink: Send + Sync {
    fn show(&self, notice: &NoticeSpec) -> Result<(), String>;
    fn withdraw(&self, id: &str) -> Result<(), String>;
    fn capability(&self) -> Availability;
    fn reason(&self) -> Option<String>;
    fn start(&self) {}
    fn shutdown(&self) {}
    /// Why the platform refused the last notice, when it said so afterwards.
    fn last_error(&self) -> Option<String> {
        None
    }
    /// Re-read the platform permission (it can change while Agendar runs).
    fn refresh(&self) {}
}

/// The only approval a notification may offer: easy effective risk, current
/// recommendation with confidence med/high, still open and unanswered.
///
/// Returns the recommended option and its label.
pub fn approve_eligible(
    need: &NeedsYouItem,
    decision: Option<&DecisionState>,
) -> Option<(OptionId, String)> {
    if need.kind != NeedsKind::Decide {
        return None;
    }
    let decision = decision?;
    if decision.lifecycle != DecisionLifecycle::Open
        || decision.effective_receipt.is_some()
        || decision.superseded_by.is_some()
        || decision.withdrawn_by.is_some()
    {
        return None;
    }
    if !matches!(
        decision.card.recommended.confidence,
        agendar_core::model::Confidence::Med | agendar_core::model::Confidence::High
    ) {
        return None;
    }
    let (risk, external, hits) = risk_scan(&decision.card);
    if risk != agendar_core::model::Reversibility::Easy || external || !hits.is_empty() {
        return None;
    }
    let recommended = decision
        .card
        .options
        .iter()
        .find(|option| option.id == decision.card.recommended.option)?;
    Some((recommended.id.clone(), recommended.label.clone()))
}

/// Buttons a question's notice may offer: every option of an open, easy
/// (reversible, local, no risky words) question, recommended first, at most
/// three. Each button carries the option's own label, so pressing one is an
/// explicit choice. Costly and irreversible questions offer Open only.
pub fn choice_options(
    need: &NeedsYouItem,
    decision: Option<&DecisionState>,
) -> Vec<(OptionId, String)> {
    if need.kind != NeedsKind::Decide {
        return Vec::new();
    }
    let Some(decision) = decision else {
        return Vec::new();
    };
    if decision.lifecycle != DecisionLifecycle::Open
        || decision.effective_receipt.is_some()
        || decision.superseded_by.is_some()
        || decision.withdrawn_by.is_some()
    {
        return Vec::new();
    }
    let (risk, external, hits) = risk_scan(&decision.card);
    if risk != agendar_core::model::Reversibility::Easy || external || !hits.is_empty() {
        return Vec::new();
    }
    let recommended = &decision.card.recommended.option;
    let mut options: Vec<&DecisionOption> = decision.card.options.iter().collect();
    options.sort_by_key(|option| &option.id != recommended);
    options
        .into_iter()
        .take(agendar_notify::MAX_CHOICES)
        .map(|option| (option.id.clone(), option.label.clone()))
        .collect()
}

/// Whether an item is announced at all: every open question (once per
/// revision), even while its agent still has other work, and every rescue row.
/// Confirm/review rows are in-app only.
pub fn notice_eligible(need: &NeedsYouItem) -> bool {
    matches!(need.kind, NeedsKind::Decide | NeedsKind::Rescue)
}

/// An agent can do nothing until this question is answered.
fn stalls_an_agent(need: &NeedsYouItem) -> bool {
    notification_eligible(need) && need.kind == NeedsKind::Decide
}

/// Build the owned notice for one need.
pub fn notice_for(
    id: String,
    project_title: &str,
    need: &NeedsYouItem,
    decision: Option<&DecisionState>,
    lang: Lang,
) -> NoticeSpec {
    NoticeSpec {
        id,
        title: title_for(project_title, need, lang),
        body: question_text(need, lang),
        choices: choice_options(need, decision)
            .into_iter()
            .map(|(_, label)| label)
            .collect(),
        project: need.project.as_str().to_owned(),
        item_id: need.id.clone(),
    }
}

fn title_for(project_title: &str, need: &NeedsYouItem, lang: Lang) -> String {
    let template = match need.kind {
        NeedsKind::Rescue => lang.pick(
            "Rescue needed — {project}",
            "需要救援 — {project}",
            "需要救援 — {project}",
        ),
        _ => lang.pick(
            "{project} needs you",
            "{project} 需要你",
            "{project} 需要你",
        ),
    };
    fill(template, &[("project", project_title)])
}

/// The need's question: an agent's own words as written, a question the core
/// wrote (rescue, review) in the person's language.
fn question_text(need: &NeedsYouItem, lang: Lang) -> String {
    let Some(msg) = &need.question_msg else {
        return need.question.clone();
    };
    let text = |name: &str| match msg.args.get(name) {
        Some(MsgArg::Text(value)) => value.clone(),
        _ => String::new(),
    };
    let template = match msg.key.as_str() {
        "need.q.handoff" => lang.pick(
            "Hand {node} to the next session?",
            "把 {node} 交给下一个会话？",
            "把 {node} 交給下一個工作階段？",
        ),
        "need.q.conflict" => lang.pick(
            "Who should continue {node}?",
            "谁来继续 {node}？",
            "誰來繼續 {node}？",
        ),
        "need.q.cycle" => lang.pick(
            "Remove {from} → {to} to repair the plan?",
            "移除 {from} → {to} 来修复计划？",
            "移除 {from} → {to} 來修復計畫？",
        ),
        "need.q.review" => lang.pick(
            "Review {name}: does it do what you asked?",
            "审核 {name}：它做到你要求的了吗？",
            "審核 {name}：它做到你要求的了嗎？",
        ),
        _ => return need.question.clone(),
    };
    fill(
        template,
        &[
            ("node", &text("node")),
            ("from", &text("from")),
            ("to", &text("to")),
            ("name", &text("name")),
        ],
    )
}

/// The option a notice's approve action would answer, resolved against the
/// current card at display time.
pub fn recommended_option(decision: &DecisionState) -> Option<&DecisionOption> {
    decision
        .card
        .options
        .iter()
        .find(|option| option.id == decision.card.recommended.option)
}

// ---------------------------------------------------------------------
// Progress notices
// ---------------------------------------------------------------------

/// How long a project must have no live running step before "work stopped" is
/// announced, so the gap between two steps never notifies.
pub const STOP_QUIET_MS: i64 = 180_000;

/// What the host remembers about one project's progress between ticks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgressMark {
    finished_milestones: BTreeSet<String>,
    running: bool,
    /// A run is open from the first running step until its stop is settled.
    run_open: bool,
    /// Steps already finished when the run began.
    finished_at_run_start: BTreeSet<String>,
    idle_since: Option<UtcTime>,
}

/// The progress facts of one project at one tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressFacts {
    pub finished_milestones: BTreeSet<String>,
    pub running: bool,
    /// Finished steps, node → title.
    pub finished: BTreeMap<String, String>,
    pub total: usize,
    /// An agent is stalled on a question: the decision notice already speaks.
    pub waits_on_human: bool,
}

impl ProgressFacts {
    pub fn of(project: &ProjectState) -> Self {
        let mut by_milestone: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        let mut finished = BTreeMap::new();
        let mut total = 0;
        let mut running = false;
        for feature in project.features.values() {
            if feature.progress == Progress::Dropped {
                continue;
            }
            let done = matches!(feature.progress, Progress::Done | Progress::Verified);
            total += 1;
            if done {
                finished.insert(feature.node.as_str().to_owned(), feature.title.clone());
            }
            running |= feature.progress == Progress::Doing && feature.liveness == Liveness::Live;
            if let Some(milestone) = feature.milestone.as_deref() {
                let entry = by_milestone.entry(milestone).or_default();
                entry.0 += usize::from(done);
                entry.1 += 1;
            }
        }
        Self {
            finished_milestones: by_milestone
                .into_iter()
                .filter(|(_, (done, all))| *all > 0 && done == all)
                .map(|(name, _)| name.to_owned())
                .collect(),
            running,
            finished,
            total,
            waits_on_human: project.needs_you.iter().any(stalls_an_agent),
        }
    }
}

impl ProgressMark {
    /// The first sight of a project: remember, never announce.
    pub fn seed(facts: &ProgressFacts) -> Self {
        Self {
            finished_milestones: facts.finished_milestones.clone(),
            running: facts.running,
            run_open: facts.running,
            finished_at_run_start: facts.finished.keys().cloned().collect(),
            idle_since: None,
        }
    }
}

/// Advance one project's mark and return at most one notice `(title, body)`:
/// a milestone that just finished, or work that stopped (no live running step
/// for [`STOP_QUIET_MS`]), or both in one notice. The title names the project;
/// the body names what finished and how much is left.
pub fn progress_notice(
    project_title: &str,
    mark: &mut ProgressMark,
    facts: &ProgressFacts,
    now: UtcTime,
    lang: Lang,
) -> Option<(String, String)> {
    let milestones: Vec<String> = facts
        .finished_milestones
        .difference(&mark.finished_milestones)
        .cloned()
        .collect();
    mark.finished_milestones = facts.finished_milestones.clone();
    let mut stopped: Option<Vec<&str>> = None;
    if facts.running {
        if !mark.run_open {
            mark.run_open = true;
            mark.finished_at_run_start = facts.finished.keys().cloned().collect();
        }
        mark.running = true;
        mark.idle_since = None;
    } else {
        if mark.running {
            mark.running = false;
            mark.idle_since = Some(now);
        }
        if mark.run_open
            && mark
                .idle_since
                .is_some_and(|since| now.millis() - since.millis() >= STOP_QUIET_MS)
        {
            mark.run_open = false;
            mark.idle_since = None;
            if !facts.waits_on_human {
                stopped = Some(
                    facts
                        .finished
                        .iter()
                        .filter(|(node, _)| !mark.finished_at_run_start.contains(*node))
                        .map(|(_, title)| title.as_str())
                        .collect(),
                );
            }
        }
    }
    let left = facts.total.saturating_sub(facts.finished.len());
    let count = left.to_string();
    let remaining = match left {
        0 => lang.pick("nothing left", "没有剩余", "沒有剩餘").to_owned(),
        1 => lang
            .pick("1 step left", "还剩 1 步", "還剩 1 步")
            .to_owned(),
        _ => fill(
            lang.pick("{n} steps left", "还剩 {n} 步", "還剩 {n} 步"),
            &[("n", &count)],
        ),
    };
    let project = fill(
        lang.pick("Project {title}", "项目 {title}", "專案 {title}"),
        &[("title", project_title)],
    );
    let stopped_title = lang.pick(
        "{project}: agents stopped",
        "{project}：智能体已停止",
        "{project}：AI 代理已停止",
    );
    match (milestones.as_slice(), stopped) {
        ([], None) => None,
        ([], Some(done)) => Some((
            fill(stopped_title, &[("project", &project)]),
            if done.is_empty() {
                fill(
                    lang.pick(
                        "Nothing finished this run · {remaining}",
                        "本轮没有完成任何步骤 · {remaining}",
                        "本輪沒有完成任何步驟 · {remaining}",
                    ),
                    &[("remaining", &remaining)],
                )
            } else {
                fill(
                    lang.pick(
                        "Done: {steps} · {remaining}",
                        "已完成：{steps} · {remaining}",
                        "已完成：{steps} · {remaining}",
                    ),
                    &[
                        ("steps", &name_steps(&done, lang)),
                        ("remaining", &remaining),
                    ],
                )
            },
        )),
        (names, stopped) => {
            let finished = facts.finished.len().to_string();
            let total = facts.total.to_string();
            let mut body = fill(
                lang.pick(
                    "{finished} of {total} steps done · {remaining}",
                    "已完成 {finished}/{total} 步 · {remaining}",
                    "已完成 {finished}/{total} 步 · {remaining}",
                ),
                &[
                    ("finished", &finished),
                    ("total", &total),
                    ("remaining", &remaining),
                ],
            );
            if stopped.is_some() {
                body.push_str(lang.pick(
                    " · agents stopped",
                    " · 智能体已停止",
                    " · AI 代理已停止",
                ));
            }
            let names = names.join(lang.pick(", ", "、", "、"));
            Some((
                fill(
                    lang.pick(
                        "{project}: {names} finished",
                        "{project}：{names} 已完成",
                        "{project}：{names} 已完成",
                    ),
                    &[("project", &project), ("names", &names)],
                ),
                body,
            ))
        }
    }
}

/// Up to two step titles, then a count: `A, B +3 more`.
fn name_steps(titles: &[&str], lang: Lang) -> String {
    const MAX_TITLE: usize = 48;
    let short = |title: &str| -> String {
        if title.chars().count() <= MAX_TITLE {
            title.to_owned()
        } else {
            let cut: String = title.chars().take(MAX_TITLE - 1).collect();
            format!("{}…", cut.trim_end())
        }
    };
    let mut text = titles
        .iter()
        .take(2)
        .map(|title| short(title))
        .collect::<Vec<_>>()
        .join(lang.pick(", ", "、", "、"));
    if titles.len() > 2 {
        let more = (titles.len() - 2).to_string();
        text.push_str(&fill(
            lang.pick(" +{n} more", "，另有 {n} 项", "，另有 {n} 項"),
            &[("n", &more)],
        ));
    }
    text
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    fn at(minute: i64) -> UtcTime {
        UtcTime::parse("2026-11-06T10:00:00.000Z")
            .expect("time")
            .add_millis(minute * 60_000)
    }

    /// `finished` steps s0..s{n}, titled "Step i".
    fn facts(running: bool, finished: usize, milestones: &[&str]) -> ProgressFacts {
        ProgressFacts {
            finished_milestones: milestones.iter().map(|name| (*name).to_owned()).collect(),
            running,
            finished: (0..finished)
                .map(|i| (format!("s{i:02}"), format!("Step {i}")))
                .collect(),
            total: 10,
            waits_on_human: false,
        }
    }

    #[test]
    fn a_short_gap_between_steps_stays_silent_and_a_real_stop_names_what_finished() {
        let mut mark = ProgressMark::seed(&facts(true, 2, &[]));
        assert_eq!(
            progress_notice("P", &mut mark, &facts(false, 3, &[]), at(0), Lang::En),
            None
        );
        assert_eq!(
            progress_notice("P", &mut mark, &facts(false, 3, &[]), at(2), Lang::En),
            None
        );
        assert_eq!(
            progress_notice("P", &mut mark, &facts(true, 3, &[]), at(2), Lang::En),
            None
        );
        assert_eq!(
            progress_notice("P", &mut mark, &facts(false, 5, &[]), at(10), Lang::En),
            None
        );
        let stop = progress_notice("P", &mut mark, &facts(false, 5, &[]), at(13), Lang::En);
        assert_eq!(
            stop,
            Some((
                "Project P: agents stopped".to_owned(),
                "Done: Step 2, Step 3 +1 more · 5 steps left".to_owned()
            ))
        );
        assert_eq!(
            progress_notice("P", &mut mark, &facts(false, 5, &[]), at(30), Lang::En),
            None,
            "once per stop"
        );
        // The next run counts from its own start.
        assert_eq!(
            progress_notice("P", &mut mark, &facts(true, 5, &[]), at(40), Lang::En),
            None
        );
        assert_eq!(
            progress_notice("P", &mut mark, &facts(false, 6, &[]), at(41), Lang::En),
            None
        );
        let stop = progress_notice("P", &mut mark, &facts(false, 6, &[]), at(45), Lang::En);
        assert_eq!(
            stop.map(|(_, body)| body),
            Some("Done: Step 5 · 4 steps left".to_owned())
        );
        assert_eq!(
            progress_notice("P", &mut mark, &facts(true, 6, &[]), at(50), Lang::En),
            None
        );
        assert_eq!(
            progress_notice("P", &mut mark, &facts(false, 6, &[]), at(51), Lang::En),
            None
        );
        let idle = progress_notice("P", &mut mark, &facts(false, 6, &[]), at(55), Lang::En);
        assert_eq!(
            idle.map(|(_, body)| body),
            Some("Nothing finished this run · 4 steps left".to_owned())
        );
    }

    #[test]
    fn a_milestone_announces_once_and_folds_in_a_stop() {
        let mut mark = ProgressMark::seed(&facts(true, 2, &["MVP"]));
        assert_eq!(
            progress_notice("P", &mut mark, &facts(true, 2, &["MVP"]), at(1), Lang::En),
            None,
            "already finished when first seen"
        );
        let done = progress_notice(
            "P",
            &mut mark,
            &facts(true, 6, &["MVP", "Beta"]),
            at(2),
            Lang::En,
        );
        assert_eq!(
            done,
            Some((
                "Project P: Beta finished".to_owned(),
                "6 of 10 steps done · 4 steps left".to_owned()
            ))
        );
        assert_eq!(
            progress_notice(
                "P",
                &mut mark,
                &facts(false, 6, &["MVP", "Beta"]),
                at(3),
                Lang::En
            ),
            None
        );
        let both = progress_notice(
            "P",
            &mut mark,
            &facts(false, 10, &["MVP", "Beta", "GA"]),
            at(7),
            Lang::En,
        );
        assert_eq!(
            both,
            Some((
                "Project P: GA finished".to_owned(),
                "10 of 10 steps done · nothing left · agents stopped".to_owned()
            ))
        );
    }

    #[test]
    fn progress_notices_follow_the_language() {
        let mut mark = ProgressMark::seed(&facts(true, 2, &[]));
        let done = progress_notice(
            "P",
            &mut mark,
            &facts(true, 6, &["MVP"]),
            at(2),
            Lang::ZhHans,
        );
        assert_eq!(
            done,
            Some((
                "项目 P：MVP 已完成".to_owned(),
                "已完成 6/10 步 · 还剩 4 步".to_owned()
            ))
        );
        assert_eq!(
            name_steps(&["A", "B", "C"], Lang::ZhHant),
            "A、B，另有 1 項"
        );
    }

    #[test]
    fn a_stop_while_an_agent_waits_on_you_is_left_to_the_decision_notice() {
        let mut mark = ProgressMark::seed(&facts(true, 1, &[]));
        let waiting = ProgressFacts {
            waits_on_human: true,
            ..facts(false, 1, &[])
        };
        assert_eq!(
            progress_notice("P", &mut mark, &waiting, at(0), Lang::En),
            None
        );
        assert_eq!(
            progress_notice("P", &mut mark, &waiting, at(5), Lang::En),
            None
        );
        assert_eq!(
            progress_notice("P", &mut mark, &facts(false, 1, &[]), at(9), Lang::En),
            None
        );
    }

    #[test]
    fn long_step_titles_are_shortened() {
        let long = "A".repeat(60);
        assert_eq!(name_steps(&[long.as_str()], Lang::En).chars().count(), 48);
        assert!(name_steps(&[long.as_str()], Lang::En).ends_with('…'));
    }
}
