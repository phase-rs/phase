//! Tests for the reanimator / graveyard-recursion feature detector. Live in a
//! sibling test module (declared from `features/tests/mod.rs`) so
//! `features/reanimator.rs` stays implementation-only and SOURCE-classified.
//!
//! Detection is verified structurally — every test builds a `CardFace` AST and
//! asserts the detector's counts. No card-name classification is used.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, ControllerRef,
    DiscardSelfScope, Effect, QuantityExpr, TargetFilter, TriggerDefinition, TypeFilter,
    TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::mana::ManaCost;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};

use crate::features::reanimator::{
    detect, effects_include_reanimation, is_graveyard_enabler, is_reanimation_payoff,
    is_reanimation_target, COMMITMENT_FLOOR, REANIMATION_TARGET_MV_FLOOR,
};

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

fn spell(effect: Effect) -> AbilityDefinition {
    AbilityDefinition::new(AbilityKind::Spell, effect)
}

/// A reanimation effect: graveyard → battlefield, for the given target body.
fn reanimation_effect(target: TargetFilter) -> Effect {
    Effect::ChangeZone {
        origin: Some(Zone::Graveyard),
        destination: Zone::Battlefield,
        target,
        owner_library: false,
        enter_transformed: false,
        enters_under: Some(ControllerRef::You),
        enter_tapped: EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: Vec::new(),
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
    }
}

fn creature_target() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature())
}

fn vehicle_target() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype("Vehicle".to_string())))
}

/// "You mill N cards" — a self-mill enabler.
fn self_mill_effect() -> Effect {
    Effect::Mill {
        count: QuantityExpr::Fixed { value: 3 },
        target: TargetFilter::Controller,
        destination: Zone::Graveyard,
    }
}

/// "You discard a card" — a self-discard (looting) enabler.
fn self_discard_effect() -> Effect {
    Effect::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
        selection: CardSelectionMode::Chosen,
        unless_filter: None,
        filter: None,
    }
}

/// A "Reanimate"-shape sorcery: one Spell ability that reanimates a creature.
fn reanimate_spell(name: &str) -> CardFace {
    let mut face = card_face(name, vec![CoreType::Sorcery]);
    face.abilities
        .push(spell(reanimation_effect(creature_target())));
    face
}

/// A high-mana creature body worth reanimating.
fn fat_creature(name: &str, mana_value: u32) -> CardFace {
    let mut face = card_face(name, vec![CoreType::Creature]);
    face.mana_cost = ManaCost::generic(mana_value);
    face
}

fn vanilla(name: &str) -> CardFace {
    let mut face = card_face(name, vec![CoreType::Creature]);
    face.mana_cost = ManaCost::generic(2);
    face
}

// ─── payoff detection ─────────────────────────────────────────────────────────

#[test]
fn reanimation_spell_is_payoff() {
    let f = detect(&[entry(reanimate_spell("Reanimate"), 1)]);
    assert_eq!(f.reanimation_count, 1);
}

#[test]
fn trigger_borne_reanimation_is_payoff() {
    // Greasefang-shape: the reanimation lives in a trigger's executed chain, not
    // a Spell ability. The detector is trigger-mode-agnostic — it walks the
    // executed effect chain.
    let mut face = card_face("Okiba Boss", vec![CoreType::Creature]);
    face.triggers.push(
        TriggerDefinition::new(TriggerMode::Attacks)
            .execute(spell(reanimation_effect(vehicle_target()))),
    );
    assert!(is_reanimation_payoff(&face));
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.reanimation_count, 1);
}

#[test]
fn vehicle_reanimation_is_payoff() {
    let mut face = card_face("Vehicle Raiser", vec![CoreType::Sorcery]);
    face.abilities
        .push(spell(reanimation_effect(vehicle_target())));
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.reanimation_count, 1);
}

#[test]
fn non_graveyard_changezone_is_not_payoff() {
    // "Put target creature from your hand onto the battlefield" (origin Hand) is
    // a cheat-into-play effect but NOT reanimation — origin must be the graveyard.
    let mut face = card_face("Sneak Attack", vec![CoreType::Sorcery]);
    face.abilities.push(spell(Effect::ChangeZone {
        origin: Some(Zone::Hand),
        destination: Zone::Battlefield,
        target: creature_target(),
        owner_library: false,
        enter_transformed: false,
        enters_under: Some(ControllerRef::You),
        enter_tapped: EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: Vec::new(),
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
    }));
    assert!(!is_reanimation_payoff(&face));
}

#[test]
fn opponent_scoped_reanimation_ignored() {
    let mut face = card_face("Opponent's Boon", vec![CoreType::Sorcery]);
    face.abilities
        .push(spell(reanimation_effect(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::Opponent),
        ))));
    assert!(!is_reanimation_payoff(&face));
    let f = detect(&[entry(face, 1)]);
    assert_eq!(f.reanimation_count, 0);
}

// ─── enabler detection ────────────────────────────────────────────────────────

#[test]
fn self_mill_is_enabler() {
    let mut face = card_face("Stitcher", vec![CoreType::Creature]);
    face.abilities.push(spell(self_mill_effect()));
    assert!(is_graveyard_enabler(&face));
}

#[test]
fn self_discard_is_enabler() {
    let mut face = card_face("Faithless Looting", vec![CoreType::Sorcery]);
    face.abilities.push(spell(self_discard_effect()));
    assert!(is_graveyard_enabler(&face));
}

#[test]
fn discard_cost_outlet_is_enabler() {
    // "Discard a card: draw a card" — the discard is the ability *cost*, caught
    // via `CostCategory::Discards` (the looting outlets reanimator decks run).
    let mut face = card_face("Looter Outlet", vec![CoreType::Creature]);
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    ability.cost = Some(AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        filter: None,
        selection: CardSelectionMode::Chosen,
        self_scope: DiscardSelfScope::FromHand,
    });
    face.abilities.push(ability);
    assert!(is_graveyard_enabler(&face));
}

#[test]
fn opponent_mill_is_not_self_enabler() {
    // "Target player mills three" (Glimpse the Unthinkable) loads an opponent's
    // graveyard, not yours — it is not a reanimator enabler.
    let mut face = card_face("Glimpse", vec![CoreType::Sorcery]);
    face.abilities.push(spell(Effect::Mill {
        count: QuantityExpr::Fixed { value: 3 },
        target: TargetFilter::Player,
        destination: Zone::Graveyard,
    }));
    assert!(!is_graveyard_enabler(&face));
}

#[test]
fn opponent_discard_is_not_self_enabler() {
    // "Target player discards" (Mind Rot) is hand disruption, not a self-enabler.
    let mut face = card_face("Mind Rot", vec![CoreType::Sorcery]);
    face.abilities.push(spell(Effect::Discard {
        count: QuantityExpr::Fixed { value: 2 },
        target: TargetFilter::Player,
        selection: CardSelectionMode::Chosen,
        unless_filter: None,
        filter: None,
    }));
    assert!(!is_graveyard_enabler(&face));
}

// ─── target detection ─────────────────────────────────────────────────────────

#[test]
fn fat_creature_is_target() {
    let face = fat_creature("Archon of Cruelty", REANIMATION_TARGET_MV_FLOOR + 1);
    assert!(is_reanimation_target(&face));
}

#[test]
fn vehicle_body_is_target() {
    let mut face = card_face("Parhelion II", vec![CoreType::Artifact]);
    face.card_type.subtypes = vec!["Vehicle".to_string()];
    face.mana_cost = ManaCost::generic(8);
    assert!(is_reanimation_target(&face));
}

#[test]
fn small_creature_is_not_target() {
    let face = fat_creature("Grizzly Bears", REANIMATION_TARGET_MV_FLOOR - 1);
    assert!(!is_reanimation_target(&face));
}

#[test]
fn noncreature_high_mv_is_not_target() {
    // An expensive sorcery is not a reanimatable body.
    let mut face = card_face("Expensive Sorcery", vec![CoreType::Sorcery]);
    face.mana_cost = ManaCost::generic(7);
    assert!(!is_reanimation_target(&face));
}

// ─── shared predicate ─────────────────────────────────────────────────────────

#[test]
fn shared_reanimation_predicate_matches_and_rejects() {
    // Single authority shared by the detector and `ReanimatorPayoffPolicy`.
    let creature = reanimation_effect(creature_target());
    let vehicle = reanimation_effect(vehicle_target());
    assert!(effects_include_reanimation(&[&creature]));
    assert!(effects_include_reanimation(&[&vehicle]));

    let mill = self_mill_effect();
    assert!(!effects_include_reanimation(&[&mill]));
    assert!(!effects_include_reanimation(&[]));
}

// ─── default / inert ──────────────────────────────────────────────────────────

#[test]
fn empty_deck_defaults() {
    let f = detect(&[]);
    assert_eq!(f.reanimation_count, 0);
    assert_eq!(f.enabler_count, 0);
    assert_eq!(f.target_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn vanilla_creature_inert() {
    let f = detect(&[entry(vanilla("Bear"), 1)]);
    assert_eq!(f.reanimation_count, 0);
    assert_eq!(f.enabler_count, 0);
    assert_eq!(f.target_count, 0);
    assert_eq!(f.commitment, 0.0);
}

// ─── calibration anchors ──────────────────────────────────────────────────────

#[test]
fn positive_calibration_real_reanimator_deck_activates() {
    // A genuine reanimator shell: 8 reanimation spells + 6 fat targets + 6
    // self-mill enablers in a 36-nonland deck must clear the activation floor.
    let mut deck = vec![entry(vanilla("Filler"), 16)];
    for i in 0..8 {
        deck.push(entry(reanimate_spell(&format!("Reanimation {i}")), 1));
    }
    for i in 0..6 {
        deck.push(entry(fat_creature(&format!("Threat {i}"), 7), 1));
    }
    for i in 0..6 {
        let mut enabler = card_face(&format!("Mill {i}"), vec![CoreType::Sorcery]);
        enabler.abilities.push(spell(self_mill_effect()));
        deck.push(entry(enabler, 1));
    }
    let f = detect(&deck);
    assert_eq!(f.reanimation_count, 8);
    assert_eq!(f.target_count, 6);
    assert_eq!(f.enabler_count, 6);
    assert!(
        f.commitment >= COMMITMENT_FLOOR,
        "real reanimator deck must activate, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_reanimation_without_targets_inert() {
    // Reanimation spells with no fat body to cheat out collapse to inert — the
    // target pillar is mandatory.
    let mut deck = vec![entry(vanilla("Filler"), 28)];
    for i in 0..8 {
        deck.push(entry(reanimate_spell(&format!("Reanimation {i}")), 1));
    }
    let f = detect(&deck);
    assert_eq!(f.reanimation_count, 8);
    assert_eq!(f.target_count, 0);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "reanimation without a target must stay inert, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_fat_creatures_without_reanimation_inert() {
    // A ramp/midrange deck full of fat creatures but no way to reanimate is not
    // a reanimator deck — the payoff pillar is mandatory.
    let mut deck = vec![entry(vanilla("Filler"), 30)];
    for i in 0..6 {
        deck.push(entry(fat_creature(&format!("Threat {i}"), 7), 1));
    }
    let f = detect(&deck);
    assert_eq!(f.reanimation_count, 0);
    assert_eq!(f.target_count, 6);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "fat creatures without reanimation must stay inert, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_single_incidental_pair_inert() {
    // One reanimation spell + one fat creature in an otherwise unrelated
    // 36-nonland deck must NOT cross the floor (the false-positive guard).
    let mut deck = vec![entry(vanilla("Filler"), 34)];
    deck.push(entry(reanimate_spell("Lone Reanimation"), 1));
    deck.push(entry(fat_creature("Lone Threat", 7), 1));
    let f = detect(&deck);
    assert_eq!(f.reanimation_count, 1);
    assert_eq!(f.target_count, 1);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "a single incidental reanimation pair must stay inert, got {}",
        f.commitment
    );
}
