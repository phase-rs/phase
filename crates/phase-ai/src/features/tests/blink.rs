//! Tests for the blink / flicker feature detector. Live in a sibling test
//! module (declared from `features/tests/mod.rs`) so `features/blink.rs` stays
//! implementation-only and SOURCE-classified.
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
use engine::types::identifiers::TrackedSetId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};

use crate::features::blink::{detect, is_etb_payoff, is_flicker_enabler, COMMITMENT_FLOOR};

fn face(name: &str, core: Vec<CoreType>) -> CardFace {
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

/// A `ChangeZone` effect with the non-flicker fields defaulted.
fn change_zone(origin: Option<Zone>, destination: Zone, target: TargetFilter) -> Effect {
    Effect::ChangeZone {
        origin,
        destination,
        target,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    }
}

fn friendly_creature() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
}

fn opponent_creature() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent))
}

fn tracked_return() -> TargetFilter {
    TargetFilter::TrackedSet {
        id: TrackedSetId(0),
    }
}

/// An exile→return ability chain (Ephemerate / Cloudshift shape): the exile is
/// the primary effect, the return is the `sub_ability`.
fn flicker_chain(exile_target: TargetFilter, return_target: TargetFilter) -> AbilityDefinition {
    let mut ability = spell(change_zone(None, Zone::Exile, exile_target));
    ability.sub_ability = Some(Box::new(spell(change_zone(
        None,
        Zone::Battlefield,
        return_target,
    ))));
    ability
}

/// A self-ETB value trigger (Mulldrifter shape): `ChangesZone`→battlefield on
/// `SelfRef`, executing a `Draw`.
fn value_etb_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::SelfRef)
        .destination(Zone::Battlefield)
        .execute(spell(Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        }))
}

// ─── flicker-enabler detection ──────────────────────────────────────────────────

#[test]
fn flicker_with_tracked_return_is_enabler() {
    // Ephemerate: exile a creature you control, then return it (TrackedSet).
    let mut c = face("Ephemerate", vec![CoreType::Instant]);
    c.abilities
        .push(flicker_chain(friendly_creature(), tracked_return()));
    assert!(is_flicker_enabler(&c));
    assert!(!is_etb_payoff(&c));
}

#[test]
fn flicker_with_parent_target_return_is_enabler() {
    // Cloudshift: exile a creature you control, then return "that card"
    // (ParentTarget anaphor instead of a tracked set).
    let mut c = face("Cloudshift", vec![CoreType::Instant]);
    c.abilities.push(flicker_chain(
        friendly_creature(),
        TargetFilter::ParentTarget,
    ));
    assert!(is_flicker_enabler(&c));
}

#[test]
fn flicker_in_trigger_chain_is_enabler() {
    // Soulherder: a combat trigger whose executed chain is the flicker.
    let mut exile = spell(change_zone(None, Zone::Exile, friendly_creature()));
    exile.sub_ability = Some(Box::new(spell(change_zone(
        None,
        Zone::Battlefield,
        TargetFilter::ParentTarget,
    ))));
    let mut c = face("Soulherder", vec![CoreType::Creature]);
    c.triggers
        .push(TriggerDefinition::new(TriggerMode::Attacks).execute(exile));
    assert!(is_flicker_enabler(&c));
    // Its trigger executes a flicker (not a value effect), so it is the engine,
    // not an ETB payoff — the two pillars stay disjoint.
    assert!(!is_etb_payoff(&c));
}

#[test]
fn exile_without_return_is_not_flicker() {
    // Pure exile removal (or a delayed-return flickerer like Flickerwisp): the
    // exile step is present but there is no immediate battlefield return.
    let mut c = face("Swords to Plowshares", vec![CoreType::Instant]);
    c.abilities
        .push(spell(change_zone(None, Zone::Exile, friendly_creature())));
    assert!(!is_flicker_enabler(&c));
}

#[test]
fn opponent_exile_then_return_is_not_flicker() {
    // Exiling an opponent's creature and returning it is tempo/removal, not a
    // value flicker — the friendly-exile guard rejects it.
    let mut c = face("Tempo Blink", vec![CoreType::Instant]);
    c.abilities
        .push(flicker_chain(opponent_creature(), tracked_return()));
    assert!(!is_flicker_enabler(&c));
}

#[test]
fn cross_ability_exile_and_return_is_not_flicker() {
    // A card with two separate, unrelated abilities — one that exiles a friendly
    // permanent and another that returns a permanent to the battlefield — must NOT
    // register as a flicker enabler. The exile and return live in independent
    // chains and do not constitute a flicker cycle.
    //
    // This test fails if is_flicker_enabler reverts to the flattened
    // collect_face_effects approach (which merged all chains into one slice and
    // triggered a false positive on the cross-ability exile+return pair).
    let mut c = face("Unrelated Exile Return", vec![CoreType::Instant]);
    // Ability A: exile only — no return step in this chain.
    c.abilities
        .push(spell(change_zone(None, Zone::Exile, friendly_creature())));
    // Ability B: return a tracked set to battlefield — no exile step in this chain.
    c.abilities.push(spell(change_zone(
        None,
        Zone::Battlefield,
        tracked_return(),
    )));
    assert!(!is_flicker_enabler(&c));
}

// ─── ETB-payoff detection ───────────────────────────────────────────────────────

#[test]
fn self_etb_draw_is_payoff() {
    // Mulldrifter: "When this creature enters, draw two cards."
    let mut c = face("Mulldrifter", vec![CoreType::Creature]);
    c.triggers.push(value_etb_trigger());
    assert!(is_etb_payoff(&c));
    assert!(!is_flicker_enabler(&c));
}

#[test]
fn friendly_creature_etb_trigger_is_payoff() {
    // "Whenever another creature you control enters, ..." engine — a Typed
    // friendly-creature filter (not SelfRef) still counts.
    let mut c = face("Soul Warden Engine", vec![CoreType::Creature]);
    c.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(friendly_creature())
            .destination(Zone::Battlefield)
            .execute(spell(Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            })),
    );
    assert!(is_etb_payoff(&c));
}

#[test]
fn etb_without_value_effect_is_not_payoff() {
    // A self-ETB whose only effect is not card-advantage / board / removal value
    // (here an attach, not in the curated value set) is not a blink payoff.
    let mut c = face("Vanilla ETB", vec![CoreType::Creature]);
    c.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield)
            .execute(spell(Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::Any,
            })),
    );
    assert!(!is_etb_payoff(&c));
}

#[test]
fn land_etb_trigger_is_not_payoff() {
    // A landfall trigger is `ChangesZone`→battlefield too, but its `valid_card`
    // is a Land, not a creature — it must not register as a blink payoff.
    let mut c = face("Landfall Payoff", vec![CoreType::Creature]);
    c.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::Typed(
                TypedFilter::land().controller(ControllerRef::You),
            ))
            .destination(Zone::Battlefield)
            .execute(spell(Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            })),
    );
    assert!(!is_etb_payoff(&c));
}

#[test]
fn non_creature_with_value_etb_is_not_payoff() {
    // The payoff body must be a creature (what a blink deck flickers). An
    // artifact with the same ETB value trigger does not count.
    let mut c = face("Etb Artifact", vec![CoreType::Artifact]);
    c.triggers.push(value_etb_trigger());
    assert!(!is_etb_payoff(&c));
}

#[test]
fn compound_and_etb_filter_with_creature_conjunct_is_payoff() {
    // An ETB trigger whose valid_card is an And filter that contains a creature
    // conjunct alongside another type conjunct (e.g. "whenever a creature or
    // artifact you control enters") must register as a payoff. The creature check
    // uses .any() over And conjuncts — at least one conjunct must name a creature.
    //
    // This test fails if etb_filter_is_self_or_friendly_creature reverts to the
    // old .all() arm, which required every And conjunct to pass the creature check
    // and wrongly rejected this filter.
    let mut c = face("Compound ETB Engine", vec![CoreType::Creature]);
    c.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature()),
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                ],
            })
            .destination(Zone::Battlefield)
            .execute(spell(Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            })),
    );
    assert!(is_etb_payoff(&c));
}

#[test]
fn compound_and_etb_filter_opponent_scoped_is_not_payoff() {
    // An And filter where a conjunct is opponent-scoped must be rejected even if
    // another conjunct names a creature — the opponent-scope check uses .all() so
    // any opponent-scoped conjunct disqualifies the whole filter.
    let mut c = face("Opponent ETB Engine", vec![CoreType::Creature]);
    c.triggers.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent),
                    ),
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                ],
            })
            .destination(Zone::Battlefield)
            .execute(spell(Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            })),
    );
    assert!(!is_etb_payoff(&c));
}

// ─── default / inert ────────────────────────────────────────────────────────────

#[test]
fn empty_deck_defaults() {
    let f = detect(&[]);
    assert_eq!(f.flicker_count, 0);
    assert_eq!(f.etb_payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

// ─── calibration anchors ────────────────────────────────────────────────────────

fn etb_creature(name: &str) -> CardFace {
    let mut c = face(name, vec![CoreType::Creature]);
    c.triggers.push(value_etb_trigger());
    c
}

fn flicker_spell(name: &str) -> CardFace {
    let mut c = face(name, vec![CoreType::Instant]);
    c.abilities
        .push(flicker_chain(friendly_creature(), tracked_return()));
    c
}

#[test]
fn positive_calibration_real_blink_deck_activates() {
    // 8 flicker enablers + 14 value-ETB creatures in a 38-nonland deck must clear
    // the floor.
    let mut deck = vec![entry(face("Filler", vec![CoreType::Creature]), 16)];
    for i in 0..8 {
        deck.push(entry(flicker_spell(&format!("Flicker {i}")), 1));
    }
    for i in 0..14 {
        deck.push(entry(etb_creature(&format!("Payoff {i}")), 1));
    }
    let f = detect(&deck);
    assert_eq!(f.flicker_count, 8);
    assert_eq!(f.etb_payoff_count, 14);
    assert!(
        f.commitment >= COMMITMENT_FLOOR,
        "real blink deck must activate, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_flicker_without_payoff_inert() {
    // A pile of flicker spells with no ETB value to re-trigger is not a blink
    // deck.
    let mut deck = vec![entry(face("Filler", vec![CoreType::Creature]), 28)];
    for i in 0..8 {
        deck.push(entry(flicker_spell(&format!("Flicker {i}")), 1));
    }
    let f = detect(&deck);
    assert_eq!(f.flicker_count, 8);
    assert_eq!(f.etb_payoff_count, 0);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "flicker without payoff must stay inert, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_payoff_without_flicker_inert() {
    // Value-ETB creatures with no way to re-trigger them are just good creatures,
    // not a blink plan.
    let mut deck = vec![entry(face("Filler", vec![CoreType::Creature]), 24)];
    for i in 0..14 {
        deck.push(entry(etb_creature(&format!("Payoff {i}")), 1));
    }
    let f = detect(&deck);
    assert_eq!(f.flicker_count, 0);
    assert_eq!(f.etb_payoff_count, 14);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "payoffs without flicker must stay inert, got {}",
        f.commitment
    );
}

#[test]
fn anti_calibration_one_incidental_flicker_inert() {
    // One incidental flicker spell + two value creatures in an otherwise
    // unrelated 36-nonland deck must not cross the floor (the false-positive
    // guard).
    let mut deck = vec![entry(face("Filler", vec![CoreType::Creature]), 33)];
    deck.push(entry(flicker_spell("Lone Cloudshift"), 1));
    deck.push(entry(etb_creature("Payoff A"), 1));
    deck.push(entry(etb_creature("Payoff B"), 1));
    let f = detect(&deck);
    assert_eq!(f.flicker_count, 1);
    assert_eq!(f.etb_payoff_count, 2);
    assert!(
        f.commitment < COMMITMENT_FLOOR,
        "one incidental flicker must stay inert, got {}",
        f.commitment
    );
}
