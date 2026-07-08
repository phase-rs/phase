//! Tests for the equipment / voltron feature detector. Live in a sibling test
//! module (declared from `features/tests/mod.rs`) so `features/equipment.rs`
//! stays implementation-only and SOURCE-classified.
//!
//! Detection is verified structurally — every test builds a `CardFace` AST and
//! asserts the detector's counts. No card-name classification is used.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, Effect, QuantityExpr,
    SearchSelectionConstraint, StaticDefinition, TargetFilter, TriggerDefinition, TypeFilter,
    TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::features::equipment::{detect, is_equipment, is_equipment_payoff, COMMITMENT_FLOOR};

fn face(name: &str, core: Vec<CoreType>, subtypes: Vec<&str>) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: core,
            subtypes: subtypes.into_iter().map(String::from).collect(),
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

fn equipment_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
        "Equipment".to_string(),
    )))
}

/// A plain Equipment — Artifact with the Equipment subtype.
fn equipment(name: &str) -> CardFace {
    face(name, vec![CoreType::Artifact], vec!["Equipment"])
}

/// "Search your library for an Equipment card" — a tutor payoff (Stoneforge).
fn search_equipment() -> Effect {
    Effect::SearchLibrary {
        source_zones: vec![Zone::Library],
        filter: equipment_filter(),
        count: QuantityExpr::Fixed { value: 1 },
        reveal: true,
        target_player: None,
        selection_constraint: SearchSelectionConstraint::None,
        split: None,
    }
}

/// "Attach an Equipment you control to a creature" — an auto-attacher payoff.
fn attach_equipment() -> Effect {
    Effect::Attach {
        attachment: equipment_filter(),
        target: TargetFilter::Any,
    }
}

/// A static that grants `equip {0}` to your Equipment (Puresteel-shape payoff).
fn equip_grant_static() -> StaticDefinition {
    let mut def = StaticDefinition::new(StaticMode::Continuous);
    def.affected = Some(equipment_filter());
    def.modifications = vec![ContinuousModification::AddKeyword {
        keyword: Keyword::Equip(ManaCost::generic(0)),
    }];
    def
}

// ─── density ──────────────────────────────────────────────────────────────────

#[test]
fn equipment_subtype_is_density() {
    let f = detect(&[entry(equipment("Bonesplitter"), 1)]);
    assert_eq!(f.equipment_count, 1);
    assert_eq!(f.payoff_count, 0);
}

#[test]
fn equipment_with_its_own_equip_ability_is_not_payoff() {
    // An Equipment's own equip ability attaches `SelfRef`; the `!Equipment` guard
    // also excludes it. It must count as density, never as support.
    let mut e = equipment("Vulshok Morningstar");
    e.abilities.push(AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Attach {
            attachment: TargetFilter::SelfRef,
            target: TargetFilter::Any,
        },
    ));
    assert!(is_equipment(&e));
    assert!(!is_equipment_payoff(&e));
}

// ─── payoff detection ─────────────────────────────────────────────────────────

#[test]
fn equipment_tutor_is_payoff() {
    let mut c = face("Stoneforge Mystic", vec![CoreType::Creature], vec![]);
    c.triggers
        .push(TriggerDefinition::new(TriggerMode::ChangesZone).execute(spell(search_equipment())));
    assert!(is_equipment_payoff(&c));
}

#[test]
fn auto_attacher_is_payoff() {
    let mut c = face("Kor Outfitter", vec![CoreType::Creature], vec![]);
    c.triggers
        .push(TriggerDefinition::new(TriggerMode::ChangesZone).execute(spell(attach_equipment())));
    assert!(is_equipment_payoff(&c));
}

#[test]
fn equipment_cast_trigger_is_payoff() {
    let mut c = face("Equipment Drawer", vec![CoreType::Creature], vec![]);
    c.triggers
        .push(TriggerDefinition::new(TriggerMode::SpellCast).valid_card(equipment_filter()));
    assert!(is_equipment_payoff(&c));
}

#[test]
fn equip_cost_grant_static_is_payoff() {
    let mut c = face("Puresteel Paladin", vec![CoreType::Creature], vec![]);
    c.static_abilities.push(equip_grant_static());
    assert!(is_equipment_payoff(&c));
}

#[test]
fn anyof_equipment_tutor_is_payoff() {
    // "Search for an Aura or Equipment card" (Steelshaper's Gift / Open the
    // Armory) — Equipment inside a `TypeFilter::AnyOf` must still be recognized.
    let mut c = face("Open the Armory", vec![CoreType::Sorcery], vec![]);
    c.abilities.push(spell(Effect::SearchLibrary {
        source_zones: vec![Zone::Library],
        filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::AnyOf(vec![
            TypeFilter::Subtype("Aura".to_string()),
            TypeFilter::Subtype("Equipment".to_string()),
        ]))),
        count: QuantityExpr::Fixed { value: 1 },
        reveal: true,
        target_player: None,
        selection_constraint: SearchSelectionConstraint::None,
        split: None,
    }));
    assert!(is_equipment_payoff(&c));
}

#[test]
fn vanilla_creature_is_not_payoff() {
    let c = face("Savannah Lions", vec![CoreType::Creature], vec![]);
    assert!(!is_equipment(&c));
    assert!(!is_equipment_payoff(&c));
}

// ─── default / inert ──────────────────────────────────────────────────────────

#[test]
fn empty_deck_defaults() {
    let f = detect(&[]);
    assert_eq!(f.equipment_count, 0);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

// ─── calibration anchors ──────────────────────────────────────────────────────

#[test]
fn positive_calibration_real_equipment_deck_activates() {
    // 14 Equipment + 8 support in a 38-nonland deck must clear the floor.
    let mut deck = vec![entry(face("Filler", vec![CoreType::Creature], vec![]), 16)];
    for i in 0..14 {
        deck.push(entry(equipment(&format!("Equip {i}")), 1));
    }
    for i in 0..8 {
        let mut c = face(&format!("Support {i}"), vec![CoreType::Creature], vec![]);
        c.triggers.push(
            TriggerDefinition::new(TriggerMode::ChangesZone).execute(spell(search_equipment())),
        );
        deck.push(entry(c, 1));
    }
    let f = detect(&deck);
    assert_eq!(f.equipment_count, 14);
    assert_eq!(f.payoff_count, 8);
    assert!(
        f.commitment >= COMMITMENT_FLOOR,
        "real equipment deck must activate, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_equipment_without_payoff_inert() {
    // A pile of swords with no support is not an equipment-matters deck.
    let mut deck = vec![entry(face("Filler", vec![CoreType::Creature], vec![]), 24)];
    for i in 0..12 {
        deck.push(entry(equipment(&format!("Equip {i}")), 1));
    }
    let f = detect(&deck);
    assert_eq!(f.equipment_count, 12);
    assert_eq!(f.payoff_count, 0);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "equipment without support must stay inert, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_payoff_without_equipment_inert() {
    // Equipment-support cards with no Equipment to enable are inert.
    let mut deck = vec![entry(face("Filler", vec![CoreType::Creature], vec![]), 30)];
    for i in 0..6 {
        let mut c = face(&format!("Support {i}"), vec![CoreType::Creature], vec![]);
        c.triggers.push(
            TriggerDefinition::new(TriggerMode::ChangesZone).execute(spell(search_equipment())),
        );
        deck.push(entry(c, 1));
    }
    let f = detect(&deck);
    assert_eq!(f.equipment_count, 0);
    assert_eq!(f.payoff_count, 6);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "support without Equipment must stay inert, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_two_incidental_swords_inert() {
    // Two swords + one tutor in an otherwise unrelated 36-nonland deck must not
    // cross the floor (the false-positive guard).
    let mut deck = vec![entry(face("Filler", vec![CoreType::Creature], vec![]), 33)];
    deck.push(entry(equipment("Sword A"), 1));
    deck.push(entry(equipment("Sword B"), 1));
    let mut tutor = face("Lone Tutor", vec![CoreType::Creature], vec![]);
    tutor
        .triggers
        .push(TriggerDefinition::new(TriggerMode::ChangesZone).execute(spell(search_equipment())));
    deck.push(entry(tutor, 1));
    let f = detect(&deck);
    assert_eq!(f.equipment_count, 2);
    assert_eq!(f.payoff_count, 1);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "two incidental swords + one tutor must stay inert, got {}",
        f.commitment
    );
}
