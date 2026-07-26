//! Unit tests for `features::cycling` — CR 702.29 "cycling matters" detection.
//! No `#[cfg(test)]` in SOURCE files; tests live here.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TriggerDefinition,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::keywords::{CyclingCost, Keyword};
use engine::types::mana::ManaCost;
use engine::types::triggers::TriggerMode;

use crate::features::cycling::*;

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

/// A cyclable card (CR 702.29a).
fn cycler(name: &str) -> CardFace {
    let mut f = face(name, CoreType::Creature);
    f.keywords = vec![Keyword::Cycling(CyclingCost::Mana(ManaCost::generic(2)))];
    f
}

fn cycled_trigger(
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
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    ))
}

/// Astral Drift shape: "whenever you cycle or discard a card, ..." — a broad,
/// controller-scoped engine on a permanent.
fn engine(name: &str) -> CardFace {
    let mut f = face(name, CoreType::Enchantment);
    f.triggers = vec![cycled_trigger(TriggerMode::CycledOrDiscarded, None, None)];
    f
}

#[test]
fn empty_deck_produces_defaults() {
    let f = detect(&[]);
    assert_eq!(f.source_count, 0);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn vanilla_deck_not_registered() {
    let f = detect(&[entry(face("Bear", CoreType::Creature), 20)]);
    assert_eq!(f.source_count, 0);
    assert_eq!(f.payoff_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn detects_cycler_source() {
    let f = detect(&[entry(cycler("Shefet Monitor"), 4)]);
    assert_eq!(f.source_count, 4);
}

#[test]
fn detects_engine_payoff() {
    let f = detect(&[entry(engine("Astral Drift"), 3)]);
    assert_eq!(f.payoff_count, 3);
}

/// A pure "when you cycle THIS card" self-bonus is a cyclable card with upside,
/// not a battlefield engine — it must not count as a payoff.
#[test]
fn self_cycle_bonus_is_not_a_payoff() {
    let mut f = cycler("Radiant Smite");
    f.triggers = vec![cycled_trigger(
        TriggerMode::Cycled,
        Some(TargetFilter::SelfRef),
        None,
    )];
    let result = detect(&[entry(f, 4)]);
    assert_eq!(result.source_count, 4, "still a cycler");
    assert_eq!(result.payoff_count, 0, "self-cycle bonus is not an engine");
}

/// An opponent-scoped "whenever an opponent cycles" punisher is not your payoff.
#[test]
fn opponent_scoped_trigger_ignored() {
    let mut f = face("Punisher", CoreType::Enchantment);
    f.triggers = vec![cycled_trigger(
        TriggerMode::CycledOrDiscarded,
        None,
        Some(TargetFilter::Opponent),
    )];
    assert_eq!(detect(&[entry(f, 2)]).payoff_count, 0);
}

/// Calibration: a dedicated cycling shell (cyclers + engines) clears the floor.
#[test]
fn committed_cycling_deck_hits_floor() {
    let deck = vec![
        entry(cycler("Cyc A"), 12),
        entry(cycler("Cyc B"), 8),
        entry(engine("Astral Drift"), 4),
        entry(engine("Drannith Stinger"), 3),
        entry(face("Plains", CoreType::Land), 24),
    ];
    let f = detect(&deck);
    assert!(
        f.commitment > 0.6,
        "committed cycling deck must clear 0.6, got {}",
        f.commitment
    );
}

/// Both pillars are mandatory: cyclers with no engine is just card smoothing.
#[test]
fn cyclers_without_engine_collapse() {
    let deck = vec![
        entry(cycler("Cyc"), 20),
        entry(face("Plains", CoreType::Land), 24),
    ];
    assert_eq!(detect(&deck).commitment, 0.0);
}

/// An engine with no cyclers never triggers → not a cycling deck.
#[test]
fn engine_without_cyclers_collapses() {
    let deck = vec![
        entry(engine("Astral Drift"), 4),
        entry(face("Plains", CoreType::Land), 24),
    ];
    assert_eq!(detect(&deck).commitment, 0.0);
}

#[test]
fn commitment_clamps_to_one() {
    let deck = vec![entry(cycler("Cyc"), 40), entry(engine("Astral Drift"), 20)];
    assert!(detect(&deck).commitment <= 1.0);
}
