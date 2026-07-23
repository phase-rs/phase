//! Unit tests for `features::poison` — structural detection + calibration
//! anchors for the CR 104.3d poison clock. No `#[cfg(test)]` in SOURCE
//! files; tests live here.

use engine::game::DeckEntry;
use engine::types::ability::{AbilityDefinition, Effect, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerCounterKind;

use crate::features::poison::*;
use engine::types::ability::{AbilityKind, QuantityExpr};
use engine::types::card::CardFace;
use engine::types::card_type::CardType;

fn creature(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        },
        ..Default::default()
    }
}

fn spell(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Instant],
            subtypes: Vec::new(),
        },
        ..Default::default()
    }
}

fn entry(card: CardFace, count: u32) -> DeckEntry {
    DeckEntry { card, count }
}

fn infect_creature(name: &str) -> CardFace {
    let mut face = creature(name);
    face.keywords = vec![Keyword::Infect];
    face
}

/// CR 122.1f: gives poison to a player who can be an opponent.
fn poison_spell(name: &str, target: TargetFilter) -> CardFace {
    let mut face = spell(name);
    face.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GivePlayerCounter {
            counter_kind: PlayerCounterKind::Poison,
            count: QuantityExpr::Fixed { value: 1 },
            target,
        },
    )];
    face
}

fn proliferate_spell(name: &str) -> CardFace {
    let mut face = spell(name);
    face.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Proliferate,
    )];
    face
}

#[test]
fn empty_deck_produces_defaults() {
    let feature = detect(&[]);
    assert_eq!(feature.source_count, 0);
    assert_eq!(feature.commitment, 0.0);
    assert!(feature.source_names.is_empty());
}

#[test]
fn vanilla_creature_not_registered() {
    let feature = detect(&[entry(creature("Grizzly Bears"), 4)]);
    assert_eq!(feature.source_count, 0);
    assert_eq!(feature.commitment, 0.0);
}

#[test]
fn detects_infect_toxic_and_poisonous_sources() {
    for keyword in [Keyword::Infect, Keyword::Toxic(1), Keyword::Poisonous(2)] {
        let mut face = creature("Source");
        face.keywords = vec![keyword.clone()];
        let feature = detect(&[entry(face, 4)]);
        assert_eq!(
            feature.source_count, 4,
            "keyword {keyword:?} must register as a poison source"
        );
    }
}

#[test]
fn detects_direct_poison_effect() {
    let feature = detect(&[entry(
        poison_spell("Virulent Wound", TargetFilter::Opponent),
        4,
    )]);
    assert_eq!(feature.direct_count, 4);
}

#[test]
fn detects_proliferate() {
    let feature = detect(&[entry(proliferate_spell("Contagion Clasp"), 2)]);
    assert_eq!(feature.proliferate_count, 2);
}

/// CR 702.164 / 702.90 / 702.70 are combat-damage abilities — a noncreature
/// face carrying the keyword is not a poison clock.
#[test]
fn noncreature_with_infect_does_not_count() {
    let mut face = spell("Not A Creature");
    face.keywords = vec![Keyword::Infect];
    let feature = detect(&[entry(face, 4)]);
    assert_eq!(feature.source_count, 0);
}

/// A drawback clause that poisons its OWN controller (Phyrexian Vatmother
/// shape) is not a payoff.
#[test]
fn self_poison_drawback_not_counted() {
    let feature = detect(&[entry(
        poison_spell("Self Poisoner", TargetFilter::Controller),
        4,
    )]);
    assert_eq!(feature.direct_count, 0);
}

/// One push per UNIQUE face, not per playset copy.
#[test]
fn source_names_dedup_per_face() {
    let feature = detect(&[entry(infect_creature("Glistener Elf"), 4)]);
    assert_eq!(feature.source_count, 4);
    assert_eq!(feature.source_names, vec!["Glistener Elf".to_string()]);
}

/// A face that is BOTH a source and a direct-poison card pushes its name once.
#[test]
fn source_and_direct_face_pushes_name_once() {
    let mut face = infect_creature("Hybrid Threat");
    face.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GivePlayerCounter {
            counter_kind: PlayerCounterKind::Poison,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Opponent,
        },
    )];
    let feature = detect(&[entry(face, 2)]);
    assert_eq!(feature.source_names.len(), 1);
}

/// Calibration anchor: Modern Infect — 12 infect creatures + 2 proliferate
/// over 37 nonland cards → strongly committed.
#[test]
fn modern_infect_hits_calibration_floor() {
    let deck = vec![
        entry(infect_creature("Glistener Elf"), 4),
        entry(infect_creature("Blighted Agent"), 4),
        entry(infect_creature("Phyrexian Crusader"), 4),
        entry(proliferate_spell("Contagion Clasp"), 2),
        entry(spell("Pump Spell"), 23),
    ];
    let feature = detect(&deck);
    assert_eq!(feature.source_count, 12);
    assert!(
        feature.commitment > 0.85,
        "Modern Infect must clear 0.85, got {}",
        feature.commitment
    );
}

/// Anti-calibration: a superfriends deck running proliferate but no poison
/// source must stay far below the policy floor.
#[test]
fn superfriends_proliferate_stays_below_floor() {
    let deck = vec![
        entry(proliferate_spell("Contagion Clasp"), 2),
        entry(spell("Planeswalker Filler"), 35),
    ];
    let feature = detect(&deck);
    assert!(
        feature.commitment < POISON_CLOCK_FLOOR,
        "proliferate alone is not a poison plan, got {}",
        feature.commitment
    );
}

#[test]
fn control_deck_below_floor() {
    let deck = vec![entry(spell("Counterspell"), 37)];
    assert_eq!(detect(&deck).commitment, 0.0);
}

#[test]
fn commitment_clamps_to_one() {
    let deck = vec![entry(infect_creature("All Infect"), 37)];
    assert_eq!(detect(&deck).commitment, 1.0);
}
