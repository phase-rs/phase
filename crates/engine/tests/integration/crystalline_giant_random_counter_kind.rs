//! Runtime regression for issue #7796 (duplicate report #8137) — Crystalline
//! Giant's beginning-of-combat trigger placed no counter at all: the parser
//! lowered its "choose ... at random ... from among <list>" clause to
//! `Effect::Unimplemented`, so the ability resolved as a no-op.
//!
//! Oracle text:
//! > At the beginning of combat on your turn, choose a kind of counter at
//! > random that this creature doesn't have on it from among flying, first
//! > strike, deathtouch, hexproof, lifelink, menace, reach, trample,
//! > vigilance, and +1/+1. Put a counter of that kind on this creature.
//!
//! Claim-to-test matrix:
//! - producer runs at all → a fresh Giant gains exactly one counter, and its
//!   kind comes from the ten printed ones (reach-guard: without the lowering
//!   nothing is placed);
//! - the exclusion narrows the CHOICE, not the placement → with nine of the ten
//!   already on it the draw has one legal option left and must land there;
//! - CR 608.2d / the printed ruling → with all ten on it the ability still
//!   triggers, and places nothing.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::keywords::KeywordKind;
use engine::types::phase::Phase;
use std::collections::HashMap;

const CRYSTALLINE_GIANT: &str =
    "At the beginning of combat on your turn, choose a kind of counter at random that this \
     creature doesn't have on it from among flying, first strike, deathtouch, hexproof, \
     lifelink, menace, reach, trample, vigilance, and +1/+1. Put a counter of that kind on \
     this creature.";

/// The ten kinds the card prints, in printed order. Written out rather than
/// read back from the parsed ability: a test that sources its expectation from
/// the code under test cannot tell a wrong list from a right one.
fn printed_kinds() -> Vec<CounterType> {
    vec![
        CounterType::Keyword(KeywordKind::Flying),
        CounterType::Keyword(KeywordKind::FirstStrike),
        CounterType::Keyword(KeywordKind::Deathtouch),
        CounterType::Keyword(KeywordKind::Hexproof),
        CounterType::Keyword(KeywordKind::Lifelink),
        CounterType::Keyword(KeywordKind::Menace),
        CounterType::Keyword(KeywordKind::Reach),
        CounterType::Keyword(KeywordKind::Trample),
        CounterType::Keyword(KeywordKind::Vigilance),
        CounterType::Plus1Plus1,
    ]
}

/// Puts a Giant carrying `preset` on the battlefield, walks the real turn into
/// beginning of combat, and returns its counters after the trigger resolved.
/// The stack assertion is the reach-guard: it separates "the trigger placed
/// nothing" from "no trigger ever fired".
fn counters_after_begin_combat(preset: &[CounterType]) -> HashMap<CounterType, u32> {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let giant = scenario
        .add_creature_from_oracle(P0, "Crystalline Giant", 3, 3, CRYSTALLINE_GIANT)
        .id();
    for kind in preset {
        scenario.with_counter(giant, kind.clone(), 1);
    }
    let mut runner = scenario.build();

    runner.pass_both_players();
    assert!(
        !runner.state().stack.is_empty(),
        "the beginning-of-combat trigger must reach the stack"
    );
    runner.advance_until_stack_empty();

    runner
        .state()
        .objects
        .get(&giant)
        .expect("Giant stays on the battlefield")
        .counters
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(kind, count)| (kind.clone(), *count))
        .collect()
}

/// CR 608.2d + CR 122.1a/CR 122.1b: the trigger draws one of the ten printed
/// kinds — nine keyword counters and +1/+1 — and puts a single counter of it
/// on the Giant.
#[test]
fn crystalline_giant_puts_one_printed_counter_kind_on_itself() {
    let counters = counters_after_begin_combat(&[]);

    assert_eq!(
        counters.len(),
        1,
        "exactly one kind must be placed, got {counters:?}"
    );
    let (kind, count) = counters.into_iter().next().expect("one entry");
    assert_eq!(count, 1, "a single counter of that kind, got {count}");
    assert!(
        printed_kinds().contains(&kind),
        "{kind:?} is not one of the ten printed kinds"
    );
}

/// CR 608.2d: "that this creature doesn't have on it" narrows the population
/// BEFORE the draw. With nine of the ten already present the random pick has a
/// single legal option, so the outcome is exact rather than probabilistic —
/// drop the exclusion and the draw ranges over all ten again.
#[test]
fn crystalline_giant_excludes_kinds_it_already_has_from_the_draw() {
    let mut preset = printed_kinds();
    let missing = preset.pop().expect("ten printed kinds");
    assert_eq!(preset.len(), 9, "nine kinds preset, one left to draw");

    let counters = counters_after_begin_combat(&preset);

    assert_eq!(
        counters.get(&missing).copied(),
        Some(1),
        "the only kind not already on it must be the one drawn, got {counters:?}"
    );
    for kind in &preset {
        assert_eq!(
            counters.get(kind).copied(),
            Some(1),
            "{kind:?} was already there once and must not be doubled"
        );
    }
    assert_eq!(counters.len(), 10, "ten kinds now, got {counters:?}");
}

/// The first of the card's three 2020-04-17 rulings: "If it has all ten kinds,
/// the ability will trigger but you won't put any counter on it." An empty
/// population is a no-op, not a draw from the unnarrowed list.
///
/// What this case does NOT pin: an unlowered clause places nothing either, so
/// this test stays green with the whole `PutChosenCounter` recipient gap back
/// in place — measured, not assumed. It falls only when the exclusion is
/// dropped, because the draw then doubles a kind already present. The lowering
/// is pinned by the other three tests, all of which fall with that gap
/// restored.
#[test]
fn crystalline_giant_with_all_ten_kinds_places_nothing() {
    let preset = printed_kinds();

    let counters = counters_after_begin_combat(&preset);

    assert_eq!(counters.len(), 10, "no eleventh kind, got {counters:?}");
    for kind in &preset {
        assert_eq!(
            counters.get(kind).copied(),
            Some(1),
            "{kind:?} must stay at one — nothing is placed once every kind is present"
        );
    }
}
