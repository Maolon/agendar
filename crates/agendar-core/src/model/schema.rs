//! What an agent may write, as `agendar schema` prints it: a valid example of
//! each JSON input, its enums and its text limits. The limits and vocabularies
//! are the validator's own constants, and the tests below decode and validate
//! every example, so this reference cannot drift from what the CLI accepts.

use serde_json::{Value, json};

use crate::model::ids::MAX_SECONDS;
use crate::model::validate::{ICON_NAMES, PROJECT_GLYPHS, limits};

/// The kinds `agendar schema` can print.
pub const KINDS: [&str; 4] = ["card", "feature", "project", "eta"];

/// One kind, or `None` for every kind under its name.
pub fn schema(kind: Option<&str>) -> Option<Value> {
    match kind {
        None => Some(json!({
            "card": card(),
            "feature": feature(),
            "project": project(),
            "eta": eta(),
        })),
        Some("card") => Some(card()),
        Some("feature") => Some(feature()),
        Some("project") => Some(project()),
        Some("eta") => Some(eta()),
        Some(_) => None,
    }
}

/// The JSON for `agendar ask --json`.
pub fn card() -> Value {
    json!({
        "example": example_card(),
        "limits": {
            "why_now": limits::CARD_WHY_NOW,
            "question": limits::CARD_QUESTION,
            "options": "2..=4, unique ids",
            "options[].label": limits::OPTION_LABEL,
            "options[].consequence": limits::OPTION_CONSEQUENCE,
            "options[].cost": limits::OPTION_COST,
            "recommended.option": "one of the option ids",
            "recommended.because": limits::RECOMMENDED_BECAUSE,
            "already_checked": "0..=3 facts",
            "already_checked[].text": limits::FACT_TEXT,
            "done_when": "1..=3 criteria, unique ids",
            "done_when[].text": limits::CRITERION_TEXT,
        },
        "enums": {
            "reversibility": ["easy", "costly", "irreversible"],
            "reach": ["local", "wide"],
            "domain": ["arch", "code", "ux"],
            "recommended.confidence": ["low", "med", "high"],
            "already_checked[].claim": ["agent_says"],
            "on_timeout": [
                {"kind": "hold"},
                {"kind": "proceed_as_assumption", "after_s": "seconds, 1..=2592000"},
            ],
            "options[].icon": ICON_NAMES.to_vec(),
        },
        "notes": [
            "Lengths are Unicode characters after trimming; one line each. Aim for the limit; up to a quarter more is accepted.",
            "Optional per option: cost, eta {min_s,max_s} in seconds, icon, image.",
            "Optional \"report\": the {\"report\": ...} blob `agendar report` returned; the human opens it in a browser.",
            "options[].image is {\"file\": \"<path>\", \"ext\": \"png|jpg|webp\"} or the {\"blob\": ...} from `agendar blob put`; the app shows option images side by side at full colour. Use one image size for every option.",
            "Irreversible or external options must use on_timeout hold.",
            "Option id `own` is reserved: the human may answer in their own words instead; the receipt then has chose.id `own` and own_answer.",
        ],
    })
}

fn example_card() -> Value {
    json!({
        "why_now": "Two storage formats both work; the choice decides how old exports stay readable.",
        "question": "Which format should exports use?",
        "reversibility": "costly",
        "reach": "local",
        "external": false,
        "domain": "arch",
        "options": [
            {
                "id": "jsonl",
                "label": "JSON lines",
                "consequence": "Streams well and diffs line by line; larger files.",
                "reversibility": "costly",
                "external": false,
                "eta": {"min_s": 1800, "max_s": 3600},
                "icon": "file-text",
            },
            {
                "id": "sqlite",
                "label": "SQLite file",
                "consequence": "Small and queryable; needs a reader to inspect.",
                "reversibility": "costly",
                "external": false,
                "eta": {"min_s": 3600, "max_s": 7200},
                "icon": "server",
            },
        ],
        "recommended": {
            "option": "jsonl",
            "confidence": "med",
            "because": "Readable without tools, and the files stay small enough here.",
        },
        "already_checked": [
            {"text": "Both formats round-trip the sample export.", "claim": "agent_says"},
        ],
        "done_when": [
            {"id": "export", "text": "Exports use the chosen format and the import reads them back."},
        ],
        "on_timeout": {"kind": "hold"},
    })
}

/// The JSON for `agendar feature declare --json`.
pub fn feature() -> Value {
    json!({
        "example": {
            "title": "Exports in the chosen format",
            "slice": "export-1",
            "demo_kind": "cli_transcript",
            "milestone": "Beta",
            "depends_on": ["core/storage"],
        },
        "limits": {
            "title": limits::FEATURE_TITLE,
            "slice": limits::FEATURE_SLICE,
            "milestone": limits::FEATURE_MILESTONE,
            "depends_on": format!("0..={} node paths", limits::FEATURE_DEPENDS_ON),
            "priority": "0..=3",
        },
        "enums": {
            "demo_kind": ["screenshot", "cli_transcript", "preview_url", "recording", "diff"],
        },
        "notes": [
            "The first declaration needs title, slice and demo_kind; later ones patch single fields.",
            "Node paths: 1..=16 slash-separated lowercase segments, such as core/storage.",
            "paused and handoff are human-only.",
            "A value a human set wins over yours.",
        ],
    })
}

/// The JSON for `agendar project declare --json`.
pub fn project() -> Value {
    json!({
        "example": {
            "title": "Export service",
            "brief": "Nightly exports of the order book for finance.",
            "goal": "Finance reads a complete export every morning.",
            "glyph": "server",
        },
        "limits": {
            "title": limits::PROJECT_TITLE,
            "brief": limits::PROJECT_BRIEF,
            "goal": limits::PROJECT_GOAL,
        },
        "enums": {
            "glyph": PROJECT_GLYPHS.to_vec(),
        },
        "notes": [
            "Declare only when `board` says the project is not declared.",
            "Set {\"archived\": true} once the goal is met and nothing is open; a human's value wins.",
            "Optional \"icon\": {\"file\": \"<absolute path>\", \"ext\": \"png|jpg|webp|svg\"}; it shows in full colour, square works best.",
        ],
    })
}

/// The duration grammar of `--eta` and `--timeout`.
pub fn eta() -> Value {
    json!({
        "grammar": "MIN..MAX; each a duration of one or more parts <n>d, <n>h, <n>m, <n>s in that order, e.g. 45m, 1h30m, 2d4h",
        "examples": ["20m..40m", "45m..1h30m", "1h..2h"],
        "max_s": MAX_SECONDS,
        "limits": {
            "progress --note": limits::NOTE,
            "pulse --message": limits::PULSE_MESSAGE,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::cards::DecisionCard;
    use crate::model::event::{PlanFeature, ProjectDeclared};

    #[test]
    fn the_example_card_is_one_the_cli_accepts() {
        let card: DecisionCard =
            serde_json::from_value(example_card()).expect("the example decodes");
        crate::model::validate::validate_card_input(&card).expect("the example validates");
    }

    #[test]
    fn every_listed_enum_value_decodes() {
        let base = example_card();
        let schema = card();
        for (field, values) in [
            ("reversibility", &schema["enums"]["reversibility"]),
            ("reach", &schema["enums"]["reach"]),
            ("domain", &schema["enums"]["domain"]),
        ] {
            for value in values.as_array().expect("values") {
                let mut card = base.clone();
                card[field] = value.clone();
                serde_json::from_value::<DecisionCard>(card)
                    .unwrap_or_else(|error| panic!("{field}={value}: {error}"));
            }
        }
        for icon in ICON_NAMES {
            let mut card = base.clone();
            card["options"][0]["icon"] = json!(icon);
            let card: DecisionCard = serde_json::from_value(card).expect("decodes");
            crate::model::validate::validate_card_input(&card)
                .unwrap_or_else(|error| panic!("icon {icon}: {error}"));
        }
        for kind in feature()["enums"]["demo_kind"].as_array().expect("kinds") {
            let mut body = feature()["example"].clone();
            body["demo_kind"] = kind.clone();
            serde_json::from_value::<PlanFeature>(body).expect("demo kind decodes");
        }
    }

    #[test]
    fn the_feature_and_project_examples_decode() {
        serde_json::from_value::<PlanFeature>(feature()["example"].clone()).expect("feature");
        serde_json::from_value::<ProjectDeclared>(project()["example"].clone()).expect("project");
    }

    #[test]
    fn the_example_texts_fit_their_limits() {
        let card = example_card();
        assert!(card["why_now"].as_str().expect("why").chars().count() <= limits::CARD_WHY_NOW);
        assert!(schema(Some("nope")).is_none());
        for kind in KINDS {
            assert!(schema(Some(kind)).is_some(), "{kind}");
        }
    }
}
