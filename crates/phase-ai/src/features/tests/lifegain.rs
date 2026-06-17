//! Tests for the lifegain-matters feature detector. Live in a sibling test
//! module (declared from `features/tests/mod.rs`) so `features/lifegain.rs`
//! stays implementation-only and SOURCE-classified.
//!
//! Detection is verified structurally — every test builds a `CardFace` AST and
//! asserts the detector's counts. No card-name classification is used.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, TargetFilter,
    TriggerDefinition, TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::keywords::Keyword;
use engine::types::triggers::TriggerMode;

use crate::features::lifegain::{detect, is_lifegain_source_parts, COMMITMENT_FLOOR};

fn card_face(name: &str, core: Vec<CoreType>) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: core,
            subtypes: Vec::new(),
        },
        ..Default::default()
    }
}

fn entry(card: CardFace, count: u32) -> DeckEntry {
    DeckEntry { card, count }
}

/// "You gain N life" effect (controller-scoped — your source).
fn gain_life_you() -> Effect {
    Effect::GainLife {
        amount: QuantityExpr::Fixed { value: 2 },
        player: TargetFilter::Controller,
    }
}

fn ability(effect: Effect) -> AbilityDefinition {
    AbilityDefinition::new(AbilityKind::Spell, effect)
}

/// A lifelink creature — a lifegain source.
fn lifelink_source(name: &str) -> CardFace {
    let mut face = card_face(name, vec![CoreType::Creature]);
    face.keywords.push(Keyword::Lifelink);
    face
}

/// A "whenever you gain life, …" payoff.
fn lifegain_payoff(name: &str) -> CardFace {
    let mut face = card_face(name, vec![CoreType::Creature]);
    face.triggers
        .push(TriggerDefinition::new(TriggerMode::LifeGained));
    face
}

fn vanilla(name: &str) -> CardFace {
    card_face(name, vec![CoreType::Creature])
}

#[test]
fn lifelink_is_source() {
    let f = detect(&[entry(lifelink_source("Vampire"), 1)]);
    assert_eq!(f.source_count, 1);
    assert_eq!(f.payoff_count, 0);
}

#[test]
fn gain_life_effect_is_source() {
    let mut face = card_face("Healing Salve", vec![CoreType::Instant]);
    face.abilities.push(ability(gain_life_you()));
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.source_count, 1);
}

#[test]
fn other_player_gain_life_is_not_your_source() {
    // "Target player gains 2 life" is not a reliable source of YOUR lifegain
    // (only a controller-scoped gain counts), so it must not register.
    let mut face = card_face("Shared Healing", vec![CoreType::Instant]);
    face.abilities.push(ability(Effect::GainLife {
        amount: QuantityExpr::Fixed { value: 2 },
        player: TargetFilter::Player,
    }));
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.source_count, 0);
}

#[test]
fn life_gained_trigger_is_payoff() {
    let f = detect(&[entry(lifegain_payoff("Ajani's Pridemate"), 1)]);
    assert_eq!(f.payoff_count, 1);
}

#[test]
fn opponent_life_gained_trigger_is_not_your_payoff() {
    let mut face = card_face("Punisher", vec![CoreType::Creature]);
    let mut trigger = TriggerDefinition::new(TriggerMode::LifeGained);
    trigger.valid_target = Some(TargetFilter::Typed(
        TypedFilter::default().controller(ControllerRef::Opponent),
    ));
    face.triggers.push(trigger);

    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.payoff_count, 0);
}

#[test]
fn trigger_borne_gain_life_is_source() {
    // A triggered "you gain N life" (e.g., Soul Warden's ETB) is a source. The
    // detector is trigger-mode-agnostic — it inspects the executed effect chain.
    let mut face = card_face("Soul Warden Variant", vec![CoreType::Creature]);
    face.triggers
        .push(TriggerDefinition::new(TriggerMode::ChangesZone).execute(ability(gain_life_you())));
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.source_count, 1);
}

#[test]
fn vanilla_creature_inert() {
    let f = detect(&[entry(vanilla("Bear"), 1)]);
    assert_eq!(f.source_count, 0);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn empty_deck_defaults() {
    let f = detect(&[]);
    assert_eq!(f.source_count, 0);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn shared_source_predicate_matches_lifelink_and_gain_life() {
    // Single authority shared by the detector and `LifegainPayoffPolicy`.
    assert!(is_lifegain_source_parts(&[Keyword::Lifelink], &[]));
    let gain = gain_life_you();
    assert!(is_lifegain_source_parts(&[], &[&gain]));
    let other = Effect::GainLife {
        amount: QuantityExpr::Fixed { value: 2 },
        player: TargetFilter::Player,
    };
    assert!(!is_lifegain_source_parts(&[], &[&other]));
    assert!(!is_lifegain_source_parts(&[Keyword::Flying], &[]));
    assert!(!is_lifegain_source_parts(&[], &[]));
}

// ─── calibration anchors ──────────────────────────────────────────────────────

#[test]
fn positive_calibration_real_lifegain_deck_activates() {
    // A genuine lifegain-matters package: 4 payoffs + 6 sources in a 40-nonland
    // deck must clear the activation floor.
    let mut deck = vec![entry(vanilla("Filler"), 30)];
    for i in 0..4 {
        deck.push(entry(lifegain_payoff(&format!("Payoff {i}")), 1));
    }
    for i in 0..6 {
        deck.push(entry(lifelink_source(&format!("Source {i}")), 1));
    }
    let f = detect(&deck);
    assert_eq!(f.payoff_count, 4);
    assert_eq!(f.source_count, 6);
    assert!(
        f.commitment >= COMMITMENT_FLOOR,
        "real lifegain deck must activate, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_single_incidental_payoff_inert() {
    // One incidental lifegain payoff in an otherwise unrelated 40-nonland deck
    // must NOT cross the floor (the false-positive guard).
    let mut deck = vec![entry(vanilla("Filler"), 39)];
    deck.push(entry(lifegain_payoff("Lone Payoff"), 1));
    let f = detect(&deck);
    assert_eq!(f.payoff_count, 1);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "a single incidental payoff must stay inert, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_incidental_lifelink_without_payoff_inert() {
    // Incidental lifelink/gain-life is common in non-lifegain decks; without
    // payoffs it must not register as a lifegain-matters commitment.
    let mut deck = vec![entry(vanilla("Filler"), 34)];
    for i in 0..6 {
        deck.push(entry(lifelink_source(&format!("Source {i}")), 1));
    }
    let f = detect(&deck);
    assert_eq!(f.payoff_count, 0);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "lifelink without payoffs must stay inert, got {}",
        f.commitment
    );
}
