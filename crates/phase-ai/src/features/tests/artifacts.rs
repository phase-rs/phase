//! Tests for the artifacts-matter feature detector. Live in a sibling test
//! module (declared from `features/tests/mod.rs`) so `features/artifacts.rs`
//! stays implementation-only and SOURCE-classified.
//!
//! Detection is verified structurally — every test builds a `CardFace` AST and
//! asserts the detector's counts. No card-name classification is used.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, PtValue, QuantityExpr, QuantityRef, TargetFilter,
    TriggerDefinition, TypeFilter, TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::keywords::Keyword;
use engine::types::triggers::TriggerMode;

use crate::features::artifacts::{detect, is_artifact_cost_payoff_parts, COMMITMENT_FLOOR};

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

fn artifact_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))
}

/// `Effect::Draw { count: <number of artifacts you control> }` — a count payoff.
fn draw_for_each_artifact() -> Effect {
    Effect::Draw {
        count: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: artifact_filter(),
            },
        },
        target: TargetFilter::Controller,
    }
}

/// `Effect::Token` producing an artifact-typed token (Treasure).
fn treasure_token_effect() -> Effect {
    Effect::Token {
        name: "Treasure".to_string(),
        power: PtValue::Fixed(0),
        toughness: PtValue::Fixed(0),
        types: vec!["Artifact".to_string()],
        colors: Vec::new(),
        keywords: Vec::new(),
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: Vec::new(),
        static_abilities: Vec::new(),
        enter_with_counters: Vec::new(),
    }
}

fn ability(effect: Effect) -> AbilityDefinition {
    AbilityDefinition::new(AbilityKind::Activated, effect)
}

fn affinity_card(name: &str) -> CardFace {
    let mut face = card_face(name, vec![CoreType::Artifact]);
    face.keywords
        .push(Keyword::Affinity(TypedFilter::new(TypeFilter::Artifact)));
    face
}

fn treasure_maker(name: &str) -> CardFace {
    let mut face = card_face(name, vec![CoreType::Enchantment]);
    face.abilities.push(ability(treasure_token_effect()));
    face
}

fn vanilla(name: &str) -> CardFace {
    card_face(name, vec![CoreType::Creature])
}

#[test]
fn counts_artifact_density() {
    let deck = vec![
        entry(card_face("Mox", vec![CoreType::Artifact]), 1),
        entry(card_face("Bear", vec![CoreType::Creature]), 1),
        entry(card_face("Island", vec![CoreType::Land]), 10),
    ];
    let f = detect(&deck);
    assert_eq!(f.artifact_count, 1);
}

#[test]
fn affinity_for_artifacts_is_payoff() {
    let mut face = card_face(
        "Affinity Spell",
        vec![CoreType::Artifact, CoreType::Creature],
    );
    face.keywords
        .push(Keyword::Affinity(TypedFilter::new(TypeFilter::Artifact)));
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.payoff_count, 1);
    assert!(f.commitment > 0.0);
}

#[test]
fn affinity_for_nonartifact_is_not_artifact_payoff() {
    // "Affinity for creatures" must NOT count as an artifacts-matter payoff.
    let mut face = card_face("Affinity Creatures", vec![CoreType::Creature]);
    face.keywords
        .push(Keyword::Affinity(TypedFilter::new(TypeFilter::Creature)));
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.payoff_count, 0);
}

#[test]
fn shared_cost_payoff_predicate_matches_affinity_and_improvise() {
    // The single authority shared by the detector and `ArtifactSynergyPolicy`.
    assert!(is_artifact_cost_payoff_parts(&[Keyword::Improvise]));
    assert!(is_artifact_cost_payoff_parts(&[Keyword::Affinity(
        TypedFilter::new(TypeFilter::Artifact)
    )]));
    // Affinity for a non-artifact type and unrelated keywords are not payoffs.
    assert!(!is_artifact_cost_payoff_parts(&[Keyword::Affinity(
        TypedFilter::new(TypeFilter::Creature)
    )]));
    assert!(!is_artifact_cost_payoff_parts(&[Keyword::Flying]));
    assert!(!is_artifact_cost_payoff_parts(&[]));
}

#[test]
fn improvise_is_payoff() {
    let mut face = card_face("Improvise Spell", vec![CoreType::Sorcery]);
    face.keywords.push(Keyword::Improvise);
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.payoff_count, 1);
}

#[test]
fn for_each_artifact_quantity_is_payoff() {
    let mut face = card_face("Metalwork", vec![CoreType::Sorcery]);
    face.abilities.push(ability(draw_for_each_artifact()));
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.payoff_count, 1);
}

#[test]
fn for_each_creature_quantity_is_not_artifact_payoff() {
    // Draw equal to creatures you control is not an artifact-count payoff.
    let mut face = card_face("Creature Draw", vec![CoreType::Sorcery]);
    face.abilities.push(ability(Effect::Draw {
        count: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature()),
            },
        },
        target: TargetFilter::Controller,
    }));
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.payoff_count, 0);
}

#[test]
fn artifact_token_generator_is_enabler() {
    let mut face = card_face("Treasure Maker", vec![CoreType::Enchantment]);
    face.abilities.push(ability(treasure_token_effect()));
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.enabler_count, 1);
    assert_eq!(f.payoff_count, 0);
}

#[test]
fn artifact_token_in_trigger_chain_is_enabler() {
    let mut face = card_face("Upkeep Treasure", vec![CoreType::Enchantment]);
    face.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone).execute(ability(treasure_token_effect())),
    );
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.enabler_count, 1);
}

#[test]
fn vanilla_creature_inert() {
    let f = detect(&[entry(card_face("Vanilla", vec![CoreType::Creature]), 1)]);
    assert_eq!(f.artifact_count, 0);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.enabler_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn empty_deck_defaults() {
    let f = detect(&[]);
    assert_eq!(f.artifact_count, 0);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.enabler_count, 0);
    assert_eq!(f.commitment, 0.0);
}

// ─── calibration anchors (maintainer-requested) ──────────────────────────────

#[test]
fn positive_calibration_real_artifacts_deck_activates() {
    // A genuine artifacts-matter package: 4 affinity payoffs + 6 Treasure
    // makers in a 40-nonland deck must clear the activation floor.
    let mut deck = vec![entry(vanilla("Filler"), 30)];
    for i in 0..4 {
        deck.push(entry(affinity_card(&format!("Payoff {i}")), 1));
    }
    for i in 0..6 {
        deck.push(entry(treasure_maker(&format!("Maker {i}")), 1));
    }
    let f = detect(&deck);
    assert_eq!(f.payoff_count, 4);
    assert_eq!(f.enabler_count, 6);
    assert!(
        f.commitment >= COMMITMENT_FLOOR,
        "real artifacts deck must activate, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_single_incidental_payoff_inert() {
    // One incidental affinity/improvise card in an otherwise non-artifact
    // 40-nonland deck must NOT activate the policy (the false-positive guard).
    let mut deck = vec![entry(vanilla("Filler"), 39)];
    deck.push(entry(affinity_card("Lone Affinity"), 1));
    let f = detect(&deck);
    assert_eq!(f.payoff_count, 1);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "a single incidental payoff must stay inert, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_treasure_makers_without_payoff_inert() {
    // Treasure makers are common in non-artifact decks; without payoffs they
    // must not activate artifacts-matter behavior.
    let mut deck = vec![entry(vanilla("Filler"), 37)];
    for i in 0..3 {
        deck.push(entry(treasure_maker(&format!("Maker {i}")), 1));
    }
    let f = detect(&deck);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.enabler_count, 3);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "treasure makers without payoffs must stay inert, got {}",
        f.commitment
    );
}

#[test]
fn commitment_clamps_to_one() {
    // A dense affinity package drives commitment to the clamp.
    let mut payoff = card_face("Affinity", vec![CoreType::Artifact]);
    payoff
        .keywords
        .push(Keyword::Affinity(TypedFilter::new(TypeFilter::Artifact)));
    let deck = vec![entry(payoff, 30)];
    let f = detect(&deck);
    assert!((f.commitment - 1.0).abs() < 1e-5);
}
