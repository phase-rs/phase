//! CR 603.1 + CR 603.2: a disjunctive trigger line whose second event head has no
//! modelled `TriggerMode` must report the card as UNSUPPORTED.
//!
//! The `becomes|is|are <complement>` trigger-event head is detected as an OPEN
//! shape, so an unmodelled complement still produces its own trigger arm. That arm
//! lands on `TriggerMode::Unknown`, which is the coverage authority's unsupported
//! marker (`game/coverage.rs`: `is_card_supported`, `check_trigger`,
//! `build_trigger_item`). Before the open head the second branch vanished and the
//! card reported as fully supported — parser coverage went green while a
//! rules-bearing branch was lost.
//!
//! NOTE ON THE VEHICLE: `is_card_supported` is private, so it cannot be asserted
//! from an integration test. `card_face_gaps` is the equivalent PUBLIC authority —
//! it applies the same `Unknown(_) || !registry.contains_key` predicate through
//! `check_trigger`, so `gaps.is_empty()` is the assertable form of "supported".
//!
//! The open shape's false-positive surface (a head match INSIDE a subject noun
//! phrase) fails in the same direction, which is the reason it is acceptable: the
//! third test here pins that.

use engine::game::coverage::card_face_gaps;
use engine::parser::parse_oracle_text;
use engine::types::card::CardFace;

fn creature_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Creature".to_string()], &[]);
    CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        ..Default::default()
    }
}

/// The honesty regression. `is turned face down` has no modelled `TriggerMode`, so
/// the card must carry a coverage gap naming that trigger.
#[test]
fn unadmitted_disjunctive_state_change_head_reports_a_coverage_gap() {
    let face = creature_face(
        "Unadmitted State Change Head Fixture",
        "When this creature enters or is turned face down, draw a card.",
    );
    // Reach-guard: the line really did split, so the gap below cannot come from a
    // line that never entered the disjunctive splitter.
    assert_eq!(
        face.triggers.len(),
        2,
        "the unadmitted head must produce its own arm, got {:?}",
        face.triggers
            .iter()
            .map(|t| format!("{}", t.mode))
            .collect::<Vec<_>>()
    );
    let gaps = card_face_gaps(&face);
    assert!(
        gaps.iter()
            .any(|g| g == "Trigger:When ~ is turned face down"),
        "the unmodelled arm must be reported as a coverage gap, got {gaps:?}"
    );
}

/// Paired positive control. Culvert Ambusher's Oracle text (verbatim, MTGJSON
/// `AtomicCards` / Scryfall) uses the SAME disjunctive shape with a modelled
/// complement, and must report ZERO gaps — otherwise the negative assertion above
/// would pass for any card at all.
#[test]
fn admitted_disjunctive_state_change_head_reports_no_coverage_gap() {
    let face = creature_face(
        "Culvert Ambusher",
        "When this creature enters or is turned face up, target creature blocks this turn if able.",
    );
    assert_eq!(face.triggers.len(), 2);
    let gaps = card_face_gaps(&face);
    assert!(
        gaps.is_empty(),
        "Culvert Ambusher must be fully supported, gaps: {gaps:?}"
    );
}

/// Coverage half of KNOWN EXPOSURE E1. When the head shape matches inside the SUBJECT
/// noun phrase, the rebuilt second arm is truncated — and this test is the proof that
/// the truncation is reported rather than silent. SYNTHETIC fixture (zero printings);
/// the parser-shape half is
/// `oracle_trigger::tests::head_shape_inside_the_subject_truncates_but_stays_honest`.
#[test]
fn subject_internal_head_shape_reports_a_coverage_gap() {
    let face = creature_face(
        "Subject Internal Head Shape Fixture",
        "Whenever a creature that is enchanted attacks or dies, draw a card.",
    );
    assert_eq!(face.triggers.len(), 2);
    let gaps = card_face_gaps(&face);
    assert!(
        gaps.iter()
            .any(|g| g == "Trigger:Whenever a creature that dies"),
        "the truncated arm must surface as a coverage gap, not silently, got {gaps:?}"
    );
}
