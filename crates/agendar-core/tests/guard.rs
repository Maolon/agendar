//! The shared answer guard: revisions that never change by accident, the
//! guard × surface table, the notice rules and every denial of an answer.

use serde_json::json;

use agendar_core::guard::{
    AnswerDenial, AnswerRequest, Choice, Guard, Refusal, Surface, approve_eligible, check_answer,
    choice_options, decision_revision, effective_risk, guard_for, need_revision, notice_eligible,
    revision, surface_allows,
};
use agendar_core::model::{
    Confidence, DecisionLifecycle, DecisionState, EventId, NeedsKind, NeedsYouItem, NodePath,
    OWN_ANSWER_MAX, OptionId, ProjectSlug, Reversibility, Sha256, State, UtcTime,
};

const PROJECT: &str = "example";

/// The reduced state a checked-in reducer fixture expects.
fn fixture_state(name: &str) -> State {
    let path = format!(
        "{}/tests/fixtures/{name}/expected.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("state")
}

fn project() -> ProjectSlug {
    ProjectSlug::parse(PROJECT).expect("project")
}

/// The fixture's one decision and its project's policy revision.
fn fixture(name: &str) -> (DecisionState, Sha256) {
    let state = fixture_state(name);
    let project = &state.projects[&project()];
    let decision = project.decisions.values().next().expect("one decision");
    (decision.clone(), project.policy.revision.clone())
}

fn need(kind: NeedsKind, source: &EventId, node: Option<&str>) -> NeedsYouItem {
    NeedsYouItem {
        id: format!("need-{}", source.as_str()),
        kind,
        project: project(),
        node: node.map(|node| NodePath::parse(node).expect("node")),
        source: source.clone(),
        question: "How should answers reach idle agents?".to_owned(),
        since: UtcTime::parse("2026-01-01T00:00:00.000Z").expect("time"),
        stalled_agents: 1,
        waiting_minutes: 5,
        weight_x2: 4,
        score_x2: 20,
        reason: "blocked".to_owned(),
        reason_msg: None,
        question_msg: None,
        unblocks: Vec::new(),
        closed_step: None,
        snoozed_until: None,
    }
}

// ---------------------------------------------------------------------------
// Revisions
// ---------------------------------------------------------------------------

/// Prepare tokens, notices and mobile client actions bind to these hashes. The
/// expected values were computed by the desktop's `projection.rs` before the
/// functions moved here; a change means every outstanding answer goes stale.
#[test]
fn revisions_of_fixed_states_are_byte_identical_to_the_desktop_ones() {
    let golden = [
        (
            "one-question",
            "3cf3e21387b2942003608cdab5e462aa492b721e608bcdb8f016786b60f9fccf",
            "e029623932e766b9a7bda8cf3db8b1b77d93fe15bcc092b9f4d7d2946f4a32d1",
        ),
        (
            "withdrawn-answer",
            "7f2a0265df3645d3e78844da5540b8c58229cfda5bc65edc93fd053c1185b682",
            "7f2a0265df3645d3e78844da5540b8c58229cfda5bc65edc93fd053c1185b682",
        ),
        (
            "answer-race",
            "28db3fef8943451ec501d9ad54bd04826d1947917afc9f7ada4913a8eb6ee88c",
            "e793f5c975a460981dc85d80cb7d7250459493ec750e1a3a02e0d635e2510b70",
        ),
    ];
    for (name, as_reduced, without_receipt) in golden {
        let (decision, policy) = fixture(name);
        assert_eq!(
            decision_revision(&project(), &decision, &policy).as_str(),
            as_reduced,
            "{name}"
        );
        // A row that shows a decision carries the decision's revision.
        let row = need(
            NeedsKind::Decide,
            &decision.id,
            Some(decision.node.as_str()),
        );
        assert_eq!(
            need_revision(&row, Some(&decision), &policy).as_str(),
            as_reduced,
            "{name} row"
        );
        let mut open = decision.clone();
        open.effective_receipt = None;
        assert_eq!(
            decision_revision(&project(), &open, &policy).as_str(),
            without_receipt,
            "{name} without a receipt"
        );
    }

    let (decision, policy) = fixture("one-question");
    let rescue = need(NeedsKind::Rescue, &decision.id, Some("core/wake"));
    assert_eq!(
        need_revision(&rescue, None, &policy).as_str(),
        "700e42eccb43bd404c31c2f97d345b29b5442abc7c409531cac8144ac8d7383d"
    );
    let review = need(NeedsKind::Review, &decision.id, None);
    assert_eq!(
        need_revision(&review, None, &policy).as_str(),
        "582981257efe362798cab4b7ca85af3e2e662242a667d83c09dc1efbd8a1c12e"
    );
    assert_eq!(
        revision(json!({"b": [1, "two", null], "a": {"z": true, "é": "ü"}})).as_str(),
        "0732619dfe8ec499c4a8318076a01da3b6db5cd33ddf5f8240b1d17312977cad"
    );
}

// ---------------------------------------------------------------------------
// Guard and surface
// ---------------------------------------------------------------------------

/// The fixture's question, open again, with the given risk facts.
fn open_decision(
    risk: Reversibility,
    confidence: Confidence,
    external: bool,
    why_now: &str,
) -> DecisionState {
    let (mut decision, _) = fixture("one-question");
    decision.lifecycle = DecisionLifecycle::Open;
    decision.effective_receipt = None;
    decision.applied_answer = None;
    decision.answer_history.clear();
    decision.ack_history.clear();
    decision.applied_sessions.clear();
    decision.card.reversibility = risk;
    decision.card.recommended.confidence = confidence;
    decision.card.external = external;
    decision.card.why_now = why_now.to_owned();
    decision
}

fn easy() -> DecisionState {
    open_decision(Reversibility::Easy, Confidence::High, false, "Blocked.")
}

fn choose() -> DecisionState {
    open_decision(Reversibility::Costly, Confidence::High, false, "Blocked.")
}

fn confirm() -> DecisionState {
    open_decision(
        Reversibility::Irreversible,
        Confidence::High,
        false,
        "Blocked.",
    )
}

#[test]
fn the_guard_follows_risk_confidence_external_effects_and_risky_words() {
    use Confidence::{High, Low, Med};
    use Reversibility::{Costly, Easy, Irreversible};
    let table = [
        (Easy, High, false, "Blocked.", Guard::Easy, Easy),
        (Easy, Med, false, "Blocked.", Guard::Easy, Easy),
        (Easy, Low, false, "Blocked.", Guard::Choose, Easy),
        (Costly, High, false, "Blocked.", Guard::Choose, Costly),
        (Costly, Low, false, "Blocked.", Guard::Choose, Costly),
        (
            Irreversible,
            High,
            false,
            "Blocked.",
            Guard::Confirm,
            Irreversible,
        ),
        (Easy, High, true, "Blocked.", Guard::Confirm, Irreversible),
        (
            Easy,
            High,
            false,
            "Deploy the migration tonight.",
            Guard::Confirm,
            Irreversible,
        ),
    ];
    for (risk, confidence, external, why_now, guard, effective) in table {
        let card = open_decision(risk, confidence, external, why_now).card;
        let case = format!("{risk:?} {confidence:?} external={external} {why_now:?}");
        assert_eq!(guard_for(&card), guard, "{case}");
        assert_eq!(effective_risk(&card), effective, "{case}");
    }
    assert_eq!(
        [Guard::Easy, Guard::Choose, Guard::Confirm].map(Guard::as_str),
        ["easy", "choose", "confirm"]
    );
}

#[test]
fn the_app_answers_every_guard_and_button_surfaces_only_easy_ones() {
    let table = [
        (Guard::Easy, Surface::App, true),
        (Guard::Easy, Surface::Notification, true),
        (Guard::Easy, Surface::Island, true),
        (Guard::Choose, Surface::App, true),
        (Guard::Choose, Surface::Notification, false),
        (Guard::Choose, Surface::Island, false),
        (Guard::Confirm, Surface::App, true),
        (Guard::Confirm, Surface::Notification, false),
        (Guard::Confirm, Surface::Island, false),
    ];
    for (guard, surface, allowed) in table {
        assert_eq!(
            surface_allows(guard, surface),
            allowed,
            "{guard:?} on {surface:?}"
        );
    }
    assert_eq!(
        Surface::ALL.map(Surface::as_str),
        ["app", "notification", "island"]
    );
}

// ---------------------------------------------------------------------------
// The answer check
// ---------------------------------------------------------------------------

/// A state whose one question is `decision`, and that question's revision.
fn state_with(decision: DecisionState) -> (State, EventId, Sha256) {
    let mut state = fixture_state("one-question");
    let slug = project();
    let project = state.projects.get_mut(&slug).expect("project");
    let policy = project.policy.revision.clone();
    let ask = decision.id.clone();
    let revision = decision_revision(&slug, &decision, &policy);
    project.decisions.clear();
    project.decisions.insert(ask.clone(), decision);
    (state, ask, revision)
}

fn option(id: &str) -> OptionId {
    OptionId::parse(id).expect("option")
}

fn refused(refusal: Refusal, revision_current: bool) -> AnswerDenial {
    AnswerDenial::Refused {
        refusal,
        revision_current,
    }
}

#[test]
fn every_guard_is_answered_from_the_app_and_only_easy_from_a_button_surface() {
    let adapters = option("adapters");
    for (decision, guard) in [
        (easy(), Guard::Easy),
        (choose(), Guard::Choose),
        (confirm(), Guard::Confirm),
    ] {
        let (state, ask, revision) = state_with(decision);
        for surface in Surface::ALL {
            let result = check_answer(
                &state,
                &AnswerRequest {
                    ask: &ask,
                    choice: Choice::Option(&adapters),
                    expected_revision: &revision,
                    surface,
                    echo_label: Some("Use harness adapters"),
                },
            );
            if surface_allows(guard, surface) {
                let checked = result.unwrap_or_else(|denial| {
                    panic!("{guard:?} on {surface:?} was denied: {denial:?}")
                });
                assert_eq!(checked.project, project());
                assert_eq!(checked.chosen.id, adapters);
                assert_eq!(checked.own_answer, None);
                assert_eq!(checked.guard, guard);
                assert_eq!(checked.revision, revision);
            } else {
                assert_eq!(
                    result,
                    Err(refused(Refusal::SurfaceNotAllowed, true)),
                    "{guard:?} on {surface:?}"
                );
            }
        }
    }
}

#[test]
fn an_answer_is_refused_with_one_typed_reason() {
    let adapters = option("adapters");
    let (state, ask, revision) = state_with(easy());
    let other = Sha256::from_bytes(b"another revision");
    fn request<'a>(
        adapters: &'a OptionId,
        ask: &'a EventId,
        expected_revision: &'a Sha256,
    ) -> AnswerRequest<'a> {
        AnswerRequest {
            ask,
            choice: Choice::Option(adapters),
            expected_revision,
            surface: Surface::App,
            echo_label: None,
        }
    }
    let unknown = EventId::parse("00000000000000000000000099").expect("id");
    assert_eq!(
        check_answer(&state, &request(&adapters, &unknown, &revision)),
        Err(refused(Refusal::NotSynced, false))
    );
    assert_eq!(
        check_answer(&state, &request(&adapters, &ask, &other)),
        Err(refused(Refusal::StaleRevision, false))
    );

    // A closed question says so whether or not its revision moved as well.
    for (lifecycle, refusal) in [
        (DecisionLifecycle::Answered, Refusal::AlreadyAnswered),
        (DecisionLifecycle::Acked, Refusal::AlreadyAnswered),
        (DecisionLifecycle::Defaulted, Refusal::AlreadyAnswered),
        (DecisionLifecycle::Amended, Refusal::AlreadyAnswered),
        (DecisionLifecycle::Withdrawn, Refusal::Withdrawn),
    ] {
        let mut closed = easy();
        closed.lifecycle = lifecycle;
        let (state, ask, revision) = state_with(closed);
        assert_eq!(
            check_answer(&state, &request(&adapters, &ask, &revision)),
            Err(refused(refusal, true)),
            "{lifecycle:?}"
        );
        assert_eq!(
            check_answer(&state, &request(&adapters, &ask, &other)),
            Err(refused(refusal, false)),
            "{lifecycle:?} at another revision"
        );
    }

    // The reduced fixtures agree: an acked answer and a withdrawn question.
    for (name, refusal) in [
        ("one-question", Refusal::AlreadyAnswered),
        ("answer-race", Refusal::AlreadyAnswered),
        ("withdrawn-answer", Refusal::Withdrawn),
    ] {
        let state = fixture_state(name);
        let (decision, policy) = fixture(name);
        let revision = decision_revision(&project(), &decision, &policy);
        let denial =
            check_answer(&state, &request(&adapters, &decision.id, &revision)).unwrap_err();
        assert_eq!(denial, refused(refusal, true), "{name}");
        assert_eq!(denial.refusal(), Some(refusal));
    }

    assert_eq!(
        Refusal::ALL.map(Refusal::as_str),
        [
            "not_synced",
            "stale_revision",
            "already_answered",
            "withdrawn",
            "surface_not_allowed"
        ]
    );
}

#[test]
fn the_choice_and_its_echoed_label_must_belong_to_the_card() {
    let adapters = option("adapters");
    let missing = option("elsewhere");
    let own = option("own");
    let check = |decision: DecisionState, choice: Choice<'_>, echo_label: Option<&str>| {
        let (state, ask, revision) = state_with(decision);
        check_answer(
            &state,
            &AnswerRequest {
                ask: &ask,
                choice,
                expected_revision: &revision,
                surface: Surface::App,
                echo_label,
            },
        )
    };

    for unknown in [&missing, &own] {
        let denial = check(easy(), Choice::Option(unknown), None).unwrap_err();
        assert_eq!(denial, AnswerDenial::UnknownOption);
        assert_eq!(denial.refusal(), None);
    }

    // An easy card may be answered without an echo; a given echo must match.
    assert!(check(easy(), Choice::Option(&adapters), None).is_ok());
    assert!(
        check(
            easy(),
            Choice::Option(&adapters),
            Some(" Use harness adapters ")
        )
        .is_ok()
    );
    assert_eq!(
        check(
            easy(),
            Choice::Option(&adapters),
            Some("Use inbox on next start")
        ),
        Err(AnswerDenial::EchoMismatch)
    );
    assert_eq!(
        check(easy(), Choice::Option(&adapters), Some("")),
        Err(AnswerDenial::EchoMismatch)
    );
    for decision in [choose(), confirm()] {
        assert_eq!(
            check(decision.clone(), Choice::Option(&adapters), None),
            Err(AnswerDenial::EchoMissing)
        );
        assert_eq!(
            check(
                decision.clone(),
                Choice::Option(&adapters),
                Some("Use inbox")
            ),
            Err(AnswerDenial::EchoMismatch)
        );
        assert!(
            check(
                decision,
                Choice::Option(&adapters),
                Some("Use harness adapters")
            )
            .is_ok()
        );
    }
    // A card whose option has no label still needs a deliberate echo.
    let mut unlabelled = choose();
    unlabelled.card.options[0].label = " ".to_owned();
    assert_eq!(
        check(unlabelled, Choice::Option(&adapters), Some("")),
        Err(AnswerDenial::EchoMissing)
    );
}

#[test]
fn own_words_are_trimmed_bounded_and_written_only_in_the_app() {
    let check = |decision: DecisionState, text: &str, surface, echo_label| {
        let (state, ask, revision) = state_with(decision);
        check_answer(
            &state,
            &AnswerRequest {
                ask: &ask,
                choice: Choice::Own(text),
                expected_revision: &revision,
                surface,
                echo_label,
            },
        )
    };
    let label = easy().card.own_option("x").label;

    let checked = check(easy(), "  Ship both.  ", Surface::App, None).expect("own words");
    assert_eq!(checked.own_answer.as_deref(), Some("Ship both."));
    assert_eq!(checked.chosen.id.as_str(), "own");
    assert_eq!(checked.chosen.consequence, "Ship both.");

    assert_eq!(
        check(easy(), "   ", Surface::App, None),
        Err(AnswerDenial::OwnAnswerMissing)
    );
    let longest = "é".repeat(OWN_ANSWER_MAX);
    assert!(check(easy(), &longest, Surface::App, None).is_ok());
    assert_eq!(
        check(easy(), &format!("{longest}é"), Surface::App, None),
        Err(AnswerDenial::OwnAnswerTooLong)
    );

    for surface in [Surface::Notification, Surface::Island] {
        assert_eq!(
            check(easy(), "Ship both.", surface, None),
            Err(refused(Refusal::SurfaceNotAllowed, true)),
            "{surface:?}"
        );
    }
    // On a choose or confirm card the echo is the own option's label.
    for decision in [choose(), confirm()] {
        assert_eq!(
            check(decision.clone(), "Ship both.", Surface::App, None),
            Err(AnswerDenial::EchoMissing)
        );
        assert!(check(decision, "Ship both.", Surface::App, Some(&label)).is_ok());
    }
}

// ---------------------------------------------------------------------------
// Notice rules
// ---------------------------------------------------------------------------

#[test]
fn a_notice_offers_answers_only_for_an_open_plainly_easy_question() {
    let labels = |decision: &DecisionState| -> Vec<String> {
        let row = need(NeedsKind::Decide, &decision.id, Some("x"));
        choice_options(&row, Some(decision))
            .into_iter()
            .map(|(id, _)| id.as_str().to_owned())
            .collect()
    };
    let approve = |decision: &DecisionState| {
        let row = need(NeedsKind::Decide, &decision.id, Some("x"));
        approve_eligible(&row, Some(decision)).map(|(id, label)| (id.as_str().to_owned(), label))
    };

    let plain = easy();
    assert_eq!(labels(&plain), ["adapters", "inbox"]);
    assert_eq!(
        approve(&plain),
        Some(("adapters".to_owned(), "Use harness adapters".to_owned()))
    );

    // The recommended option comes first, and three buttons are the most.
    let mut many = easy();
    many.card.recommended.option = option("inbox");
    for id in ["third", "fourth"] {
        let mut extra = many.card.options[0].clone();
        extra.id = option(id);
        many.card.options.push(extra);
    }
    assert_eq!(labels(&many), ["inbox", "adapters", "third"]);

    // Low confidence: each button is still an explicit choice, but there is
    // no approval of the recommendation.
    let low = open_decision(Reversibility::Easy, Confidence::Low, false, "Blocked.");
    assert_eq!(labels(&low), ["adapters", "inbox"]);
    assert_eq!(approve(&low), None);

    for risky in [
        choose(),
        confirm(),
        open_decision(Reversibility::Easy, Confidence::High, true, "Blocked."),
        open_decision(
            Reversibility::Easy,
            Confidence::High,
            false,
            "Deploy the migration tonight.",
        ),
    ] {
        assert!(labels(&risky).is_empty());
        assert_eq!(approve(&risky), None);
    }

    let mut answered = easy();
    answered.lifecycle = DecisionLifecycle::Answered;
    let mut receipted = easy();
    receipted.effective_receipt = Some(receipted.id.clone());
    let mut superseded = easy();
    superseded.superseded_by = Some(superseded.id.clone());
    let mut withdrawn = easy();
    withdrawn.withdrawn_by = Some(withdrawn.id.clone());
    for closed in [answered, receipted, superseded, withdrawn] {
        assert!(labels(&closed).is_empty());
        assert_eq!(approve(&closed), None);
    }

    // Only a question row answers; a rescue or review row is announced or not.
    let rescue = need(NeedsKind::Rescue, &plain.id, Some("x"));
    assert!(choice_options(&rescue, Some(&plain)).is_empty());
    assert_eq!(approve_eligible(&rescue, None), None);
    let question = need(NeedsKind::Decide, &plain.id, Some("x"));
    assert!(choice_options(&question, None).is_empty());
    assert!(notice_eligible(&question));
    assert!(notice_eligible(&rescue));
    for kind in [NeedsKind::Confirm, NeedsKind::Review] {
        assert!(!notice_eligible(&need(kind, &plain.id, Some("x"))));
    }
}
