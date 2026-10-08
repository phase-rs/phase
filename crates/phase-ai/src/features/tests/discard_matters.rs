//! Unit tests for `features::discard_matters` — CR 701.9 "whenever you discard"
//! detection. No `#[cfg(test)]` in SOURCE files; tests live here.

use engine::game::DeckEntry;
use engine::types::ability::CardSelectionMode;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, DiscardSelfScope, Effect, QuantityExpr,
    TargetFilter, TriggerDefinition,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::triggers::TriggerMode;

use crate::features::discard_matters::*;

fn face(name: &str, core: CoreType) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: vec![core],
            subtypes: Vec::new(),
        },
        ..Default::default()
    }
}

fn entry(card: CardFace, count: u32) -> DeckEntry {
    DeckEntry { card, count }
}

/// The rich enabler form: "discard two cards" scoped to you.
fn discard_source(name: &str) -> CardFace {
    let mut f = face(name, CoreType::Sorcery);
    f.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Discard {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
            selection: CardSelectionMode::Chosen,
            unless_filter: None,
            filter: None,
        },
    )];
    f
}

/// The older simple enabler form — a separate `Effect` variant with its own
/// resolver path. Reading only `Effect::Discard` would drop this half.
fn discard_card_source(name: &str) -> CardFace {
    let mut f = face(name, CoreType::Sorcery);
    f.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DiscardCard {
            count: 1,
            target: TargetFilter::Controller,
        },
    )];
    f
}

/// Opponent-facing discard — `hand_disruption`'s subject, not this axis.
fn opponent_discard(name: &str) -> CardFace {
    let mut f = face(name, CoreType::Sorcery);
    f.abilities = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DiscardCard {
            count: 1,
            target: TargetFilter::Opponent,
        },
    )];
    f
}

fn payoff_trigger(
    mode: TriggerMode,
    valid_card: Option<TargetFilter>,
    valid_target: Option<TargetFilter>,
) -> TriggerDefinition {
    let mut t = TriggerDefinition::new(mode);
    if let Some(vc) = valid_card {
        t = t.valid_card(vc);
    }
    if let Some(vt) = valid_target {
        t = t.valid_target(vt);
    }
    t.execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    ))
}

/// The Archfiend of Ifnir / Bone Miser shape.
fn engine_card(name: &str) -> CardFace {
    let mut f = face(name, CoreType::Creature);
    f.triggers = vec![payoff_trigger(TriggerMode::Discarded, None, None)];
    f
}

fn vanilla(name: &str) -> CardFace {
    face(name, CoreType::Creature)
}

/// `sources` outlets + `engines` payoffs, padded to 36 nonland.
fn deck(sources: u32, engines: u32) -> Vec<DeckEntry> {
    let filler = 36u32.saturating_sub(sources + engines);
    vec![
        entry(discard_source("Outlet"), sources),
        entry(engine_card("Engine"), engines),
        entry(vanilla("Filler"), filler),
    ]
}

#[test]
fn empty_deck_produces_defaults() {
    let f = detect(&[]);
    assert_eq!(f.source_count, 0);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn vanilla_creature_not_registered() {
    let f = detect(&[entry(vanilla("Bear"), 4)]);
    assert_eq!(f.source_count, 0);
    assert_eq!(f.payoff_count, 0);
}

#[test]
fn detects_rich_discard_effect_source() {
    let f = detect(&deck(18, 5));
    assert_eq!(f.source_count, 18);
}

#[test]
fn detects_simple_discard_card_effect_source() {
    // Sibling-variant coverage: `Effect::DiscardCard` is a distinct enum variant
    // with its own resolver path, and must count as an enabler too.
    let f = detect(&[
        entry(discard_card_source("Simple Outlet"), 18),
        entry(engine_card("Engine"), 5),
        entry(vanilla("Filler"), 13),
    ]);
    assert_eq!(f.source_count, 18);
    assert!(f.commitment > 0.0);
}

#[test]
fn detects_discarded_all_payoff() {
    let mut e = face("Hand Dumper", CoreType::Creature);
    e.triggers = vec![payoff_trigger(TriggerMode::DiscardedAll, None, None)];
    let f = detect(&[
        entry(discard_source("Outlet"), 18),
        entry(e, 5),
        entry(vanilla("Filler"), 13),
    ]);
    assert_eq!(f.payoff_count, 5);
}

#[test]
fn opponent_discard_is_not_a_source() {
    // `hand_disruption`'s domain — the axes must never read the same card.
    let f = detect(&[
        entry(opponent_discard("Coercion"), 18),
        entry(engine_card("Engine"), 5),
        entry(vanilla("Filler"), 13),
    ]);
    assert_eq!(f.source_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn opponent_scoped_payoff_is_not_an_engine() {
    // "Whenever an opponent discards" is a punisher for a different deck.
    let mut e = face("Punisher", CoreType::Creature);
    e.triggers = vec![payoff_trigger(
        TriggerMode::Discarded,
        None,
        Some(TargetFilter::Opponent),
    )];
    let f = detect(&[
        entry(discard_source("Outlet"), 18),
        entry(e, 5),
        entry(vanilla("Filler"), 13),
    ]);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn self_referential_discard_trigger_is_not_an_engine() {
    // Madness / "when this is discarded" fires from the card being pitched, not
    // from a battlefield engine — counting it would let a pile of madness cards
    // masquerade as an engine base.
    let mut e = face("Madness Card", CoreType::Creature);
    e.triggers = vec![payoff_trigger(
        TriggerMode::Discarded,
        Some(TargetFilter::SelfRef),
        None,
    )];
    let f = detect(&[
        entry(discard_source("Outlet"), 18),
        entry(e, 5),
        entry(vanilla("Filler"), 13),
    ]);
    assert_eq!(f.payoff_count, 0);
}

#[test]
fn cycling_trigger_is_not_counted() {
    // Boundary with `cycling_discipline`: `CycledOrDiscarded` is that policy's
    // subject; counting it here would double-score one card across two policies.
    let mut e = face("Cycler Payoff", CoreType::Creature);
    e.triggers = vec![payoff_trigger(TriggerMode::CycledOrDiscarded, None, None)];
    let f = detect(&[
        entry(discard_source("Outlet"), 18),
        entry(e, 5),
        entry(vanilla("Filler"), 13),
    ]);
    assert_eq!(f.payoff_count, 0);
}

#[test]
fn payoff_without_a_resolvable_execute_is_not_an_engine() {
    let mut e = face("No Execute", CoreType::Creature);
    e.triggers = vec![TriggerDefinition::new(TriggerMode::Discarded)];
    let f = detect(&[
        entry(discard_source("Outlet"), 18),
        entry(e, 5),
        entry(vanilla("Filler"), 13),
    ]);
    assert_eq!(f.payoff_count, 0);
}

#[test]
fn outlets_without_an_engine_collapse_commitment() {
    let f = detect(&[
        entry(discard_source("Outlet"), 18),
        entry(vanilla("Filler"), 18),
    ]);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn engine_without_outlets_collapses_commitment() {
    let f = detect(&[
        entry(engine_card("Engine"), 5),
        entry(vanilla("Filler"), 31),
    ]);
    assert_eq!(f.source_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn pitch_shell_hits_calibration_anchor() {
    // Docstring anchor: 10 outlets + 4 engines over 36 nonland → 0.878.
    let f = detect(&deck(10, 4));
    assert!(
        (f.commitment - 0.878).abs() < 0.01,
        "expected ≈0.878, got {}",
        f.commitment
    );
    assert!(f.commitment >= DISCARD_MATTERS_FLOOR);
}

#[test]
fn light_pitch_build_still_clears_the_floor() {
    // Docstring anchor: 6 outlets + 2 engines → 0.481, a real but lean plan.
    let f = detect(&deck(6, 2));
    assert!(
        (f.commitment - 0.481).abs() < 0.01,
        "expected ≈0.481, got {}",
        f.commitment
    );
    assert!(f.commitment >= DISCARD_MATTERS_FLOOR);
}

#[test]
fn incidental_rummaging_stays_below_floor() {
    // Docstring anti-anchor: 2 outlets + 1 engine over 36 nonland → 0.196.
    let f = detect(&deck(2, 1));
    assert!(
        (f.commitment - 0.196).abs() < 0.01,
        "expected ≈0.196, got {}",
        f.commitment
    );
    assert!(
        f.commitment < DISCARD_MATTERS_FLOOR,
        "expected below floor, got {}",
        f.commitment
    );
}

#[test]
fn commitment_is_format_size_neutral() {
    let sixty = detect(&deck(18, 5));
    let commander = detect(&[
        entry(discard_source("Outlet"), 31),
        entry(engine_card("Engine"), 9),
        entry(vanilla("Filler"), 23),
    ]);
    assert!(
        (sixty.commitment - commander.commitment).abs() < 0.05,
        "{} vs {}",
        sixty.commitment,
        commander.commitment
    );
}

#[test]
fn commitment_clamps_to_one() {
    let f = detect(&[
        entry(discard_source("Outlet"), 30),
        entry(engine_card("Engine"), 10),
    ]);
    assert!(f.commitment <= 1.0);
}

#[test]
fn lands_are_excluded_from_the_denominator() {
    let with_lands = detect(&[
        entry(discard_source("Outlet"), 18),
        entry(engine_card("Engine"), 5),
        entry(vanilla("Filler"), 13),
        entry(face("Swamp", CoreType::Land), 24),
    ]);
    let without = detect(&deck(18, 5));
    assert_eq!(with_lands.commitment, without.commitment);
}

// ─── review #6786: the discard-as-COST path at deck time ────────────────────

fn discard_cost(count: i32) -> AbilityCost {
    AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: count },
        filter: None,
        selection: CardSelectionMode::Chosen,
        self_scope: DiscardSelfScope::FromHand,
    }
}

/// Wild Mongrel: an activated ability whose COST is the discard.
fn cost_outlet(name: &str, cost: AbilityCost) -> CardFace {
    let mut f = face(name, CoreType::Creature);
    f.abilities = vec![AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(cost)];
    f
}

#[test]
fn detects_discard_cost_outlet() {
    let f = detect(&[
        entry(cost_outlet("Wild Mongrel", discard_cost(1)), 10),
        entry(engine_card("Engine"), 4),
        entry(vanilla("Filler"), 22),
    ]);
    assert_eq!(f.source_count, 10, "a discard COST must count as an outlet");
    assert!(f.commitment >= DISCARD_MATTERS_FLOOR);
}

#[test]
fn deck_time_counts_a_one_of_discard_cost() {
    // Deck-time asks "could this discard?", so an optional branch still marks the
    // card for archetype classification — the live seam is the strict one.
    let f = detect(&[
        entry(
            cost_outlet(
                "Optional Outlet",
                AbilityCost::OneOf {
                    costs: vec![
                        discard_cost(1),
                        AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 2 },
                        },
                    ],
                },
            ),
            10,
        ),
        entry(engine_card("Engine"), 4),
        entry(vanilla("Filler"), 22),
    ]);
    assert_eq!(f.source_count, 10);
}

#[test]
fn zero_count_discard_cost_is_still_a_deck_time_outlet() {
    // `DiscardQuantity::Any` at deck time: the count is unknowable when building.
    let f = detect(&[
        entry(cost_outlet("Weird", discard_cost(0)), 10),
        entry(engine_card("Engine"), 4),
        entry(vanilla("Filler"), 22),
    ]);
    assert_eq!(f.source_count, 10);
}
