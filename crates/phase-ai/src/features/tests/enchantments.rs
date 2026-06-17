//! Tests for the enchantments-matter feature detector. Live in a sibling test
//! module (declared from `features/tests/mod.rs`) so `features/enchantments.rs`
//! stays implementation-only and SOURCE-classified.
//!
//! Detection is verified structurally — every test builds a `CardFace` AST and
//! asserts the detector's counts. No card-name classification is used.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, TargetFilter,
    TriggerDefinition, TypeFilter, TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::features::enchantments::{detect, COMMITMENT_FLOOR};

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

fn draw_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
}

/// Enchantress: "whenever you cast an enchantment spell, draw a card."
fn enchantress_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::SpellCast)
        .valid_card(TargetFilter::Typed(TypedFilter::new(
            TypeFilter::Enchantment,
        )))
        .execute(draw_ability())
}

/// Constellation: "whenever an enchantment you control enters the battlefield."
fn constellation_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Enchantment).controller(ControllerRef::You),
        ))
        .destination(Zone::Battlefield)
        .execute(draw_ability())
}

fn enchantress(name: &str) -> CardFace {
    let mut face = card_face(name, vec![CoreType::Enchantment]);
    face.triggers.push(enchantress_trigger());
    face
}

fn enchantment(name: &str) -> CardFace {
    card_face(name, vec![CoreType::Enchantment])
}

fn vanilla(name: &str) -> CardFace {
    card_face(name, vec![CoreType::Creature])
}

#[test]
fn counts_enchantment_density() {
    let deck = vec![
        entry(enchantment("Aura"), 1),
        entry(vanilla("Bear"), 1),
        entry(card_face("Island", vec![CoreType::Land]), 10),
    ];
    let f = detect(&deck);
    assert_eq!(f.enchantment_count, 1);
}

#[test]
fn enchantress_trigger_is_payoff() {
    let f = detect(&[entry(enchantress("Enchantress"), 1)]);
    assert_eq!(f.payoff_count, 1);
    assert_eq!(f.enchantment_count, 1);
}

#[test]
fn constellation_trigger_is_payoff() {
    let mut face = card_face("Constellation Payoff", vec![CoreType::Enchantment]);
    face.triggers.push(constellation_trigger());
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.payoff_count, 1);
}

#[test]
fn compound_and_constellation_is_payoff() {
    // "Whenever a nontoken/creature enchantment you control enters" — the
    // `valid_card` is an `And` whose enchantment-you-control conjunct is combined
    // with an extra constraint. The extra conjunct only narrows the match, so the
    // trigger is still a constellation payoff (regression guard for the And arm).
    let mut face = card_face("Compound Constellation", vec![CoreType::Enchantment]);
    face.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Enchantment).controller(ControllerRef::You),
                    ),
                    TargetFilter::Typed(TypedFilter::creature()),
                ],
            })
            .destination(Zone::Battlefield)
            .execute(draw_ability()),
    );
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.payoff_count, 1);
}

#[test]
fn cast_creature_trigger_is_not_enchantment_payoff() {
    // "Whenever you cast a creature spell" must NOT count as an enchantments payoff.
    let mut face = card_face("Creature Caster", vec![CoreType::Creature]);
    face.triggers.push(
        TriggerDefinition::new(TriggerMode::SpellCast)
            .valid_card(TargetFilter::Typed(TypedFilter::creature()))
            .execute(draw_ability()),
    );
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.payoff_count, 0);
}

#[test]
fn opponent_constellation_is_not_your_payoff() {
    // An enchantment-ETB trigger scoped to an opponent's enchantments is a
    // punisher, not your constellation payoff.
    let mut face = card_face("Punisher", vec![CoreType::Enchantment]);
    face.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Enchantment).controller(ControllerRef::Opponent),
            ))
            .destination(Zone::Battlefield)
            .execute(draw_ability()),
    );
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.payoff_count, 0);
}

#[test]
fn vanilla_creature_inert() {
    let f = detect(&[entry(vanilla("Bear"), 1)]);
    assert_eq!(f.enchantment_count, 0);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn empty_deck_defaults() {
    let f = detect(&[]);
    assert_eq!(f.enchantment_count, 0);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

// ─── calibration anchors ──────────────────────────────────────────────────────

#[test]
fn positive_calibration_real_enchantress_deck_activates() {
    // 4 payoffs + 8 enchantments in a 40-nonland deck must clear the floor.
    let mut deck = vec![entry(vanilla("Filler"), 28)];
    for i in 0..4 {
        deck.push(entry(enchantress(&format!("Payoff {i}")), 1));
    }
    for i in 0..8 {
        deck.push(entry(enchantment(&format!("Enchantment {i}")), 1));
    }
    let f = detect(&deck);
    assert_eq!(f.payoff_count, 4);
    assert!(
        f.commitment >= COMMITMENT_FLOOR,
        "real enchantress deck must activate, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_single_incidental_payoff_inert() {
    // One incidental enchantment payoff in an otherwise unrelated 40-nonland
    // deck must NOT cross the floor.
    let mut deck = vec![entry(vanilla("Filler"), 39)];
    deck.push(entry(enchantress("Lone Payoff"), 1));
    let f = detect(&deck);
    assert_eq!(f.payoff_count, 1);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "a single incidental payoff must stay inert, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_enchantments_without_payoff_inert() {
    // A few incidental enchantments without payoffs must not register as a
    // lifegain-matters commitment.
    let mut deck = vec![entry(vanilla("Filler"), 34)];
    for i in 0..6 {
        deck.push(entry(enchantment(&format!("Aura {i}")), 1));
    }
    let f = detect(&deck);
    assert_eq!(f.payoff_count, 0);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "enchantments without payoffs must stay inert, got {}",
        f.commitment
    );
}
