//! Regression tests for Hunter's Insight — combat-damage → draw pipeline
//! (issue #391, follow-up to #384 / commit 6d18f5322).
//!
//! Hunter's Insight Oracle: "Choose target creature you control. Whenever that
//! creature deals combat damage to a player or planeswalker this turn, draw
//! that many cards."
//!
//! The card parses to an `Effect::TargetOnly` (target creature you control)
//! whose `SequentialSibling` sub-ability is an `Effect::CreateDelayedTrigger`.
//! The delayed trigger is a `WheneverEvent` over `DamageDone` (combat-only)
//! whose `valid_source` is `ParentTarget` (the chosen creature) and whose
//! `valid_target` is `Or[Player, Planeswalker]`. When the trigger fires it
//! draws `EventContextAmount` cards (the combat-damage amount) to the
//! controller. The draw is **mandatory** — the Oracle text has no "you may".
//!
//! These tests pin the #384 shape against regression:
//!   - Case A: targeted creature deals combat damage to a player → controller
//!     draws exactly that many cards.
//!   - Case B: parser-shape assertion — the delayed trigger's `valid_target`
//!     covers planeswalkers, `valid_source` is `ParentTarget`, `optional` is
//!     false. (The scenario harness has no planeswalker support, so the
//!     planeswalker branch is verified by shape, not runtime.)
//!   - Case C: a non-targeted decoy creature deals combat damage → no draw
//!     (the `ParentTarget` binding does not match the decoy), while the decoy
//!     still deals its combat damage (proving combat actually happened).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    DelayedTriggerCondition, Effect, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

use engine::types::player::PlayerId;

use super::rules::run_combat;

/// Count cards in a player's hand. `GameRunner` exposes `life` directly but no
/// hand accessor, so read it off the filtered game state.
fn hand_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

/// Verified Oracle text from `client/public/card-data.json` (`jq '.["hunter's insight"]'`).
const HUNTERS_INSIGHT_ORACLE: &str = "Choose target creature you control. Whenever \
    that creature deals combat damage to a player or planeswalker this turn, draw \
    that many cards.";

/// Build a scenario with Hunter's Insight in P0's hand, the targeted creature
/// ("Insight Bear", 3/3) on the battlefield, and P0's library stocked with
/// generic cards so `Draw` actually moves objects. Returns the runner, the
/// Hunter's Insight object id, and the Insight Bear object id.
fn setup() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The creature Hunter's Insight will target. Power 3 → draw 3.
    let insight_bear = scenario.add_creature(P0, "Insight Bear", 3, 3).id();

    let hunters_insight = scenario
        .add_spell_to_hand_from_oracle(P0, "Hunter's Insight", true, HUNTERS_INSIGHT_ORACLE)
        .id();

    // Stock the library so the draw is observable (an empty library makes the
    // draw a no-op and risks a draw-from-empty loss).
    for name in [
        "Library Card 1",
        "Library Card 2",
        "Library Card 3",
        "Library Card 4",
        "Library Card 5",
    ] {
        scenario.add_card_to_library_top(P0, name);
    }

    let runner = scenario.build();
    (runner, hunters_insight, insight_bear)
}

/// Cast Hunter's Insight from P0's hand, targeting `target`, and resolve the
/// spell so the delayed trigger is created. The outer `TargetOnly` slot is
/// answered with the declared `target` (CR 601.2c) — explicit so Case C's two
/// legal creatures are disambiguated — and the spell resolves to create the
/// delayed trigger (CR 603.7d).
fn cast_targeting(runner: &mut GameRunner, hunters_insight: ObjectId, target: ObjectId) {
    runner.cast(hunters_insight).target_object(target).resolve();
}

/// Case A — the targeted creature deals combat damage to a player. The
/// controller draws exactly that many cards (mandatory draw).
#[test]
fn targeted_creature_combat_damage_draws_that_many() {
    let (mut runner, hunters_insight, insight_bear) = setup();
    cast_targeting(&mut runner, hunters_insight, insight_bear);

    let hand_before = hand_count(&runner, P0);
    let life_before_p1 = runner.life(P1);

    // Insight Bear (3/3) attacks unblocked → 3 combat damage to P1 (CR 510.1b).
    run_combat(&mut runner, vec![insight_bear], vec![]);
    // CR 510.3a: the delayed trigger is placed on the stack in the combat
    // damage step — drain it so the draw resolves before asserting.
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        life_before_p1 - 3,
        "CR 510.1b / 510.2: unblocked 3/3 deals 3 combat damage to the player"
    );
    assert_eq!(
        hand_count(&runner, P0),
        hand_before + 3,
        "CR 603.7b: the delayed trigger fires and draws EventContextAmount (3) cards"
    );
}

/// Case B — parser-shape assertion. The scenario harness has no planeswalker
/// support, so planeswalker coverage is verified by the parsed card shape
/// rather than by a runtime planeswalker-attack test. This also pins the
/// `valid_source: ParentTarget` and `optional: false` shape from #384.
#[test]
fn delayed_trigger_shape_covers_planeswalker_and_parent_target() {
    let (runner, hunters_insight, _insight_bear) = setup();

    let obj = runner
        .state()
        .objects
        .get(&hunters_insight)
        .expect("Hunter's Insight present in hand");
    let ability = obj
        .abilities
        .first()
        .expect("Hunter's Insight has one parsed ability");

    // Outer effect: TargetOnly (target creature you control).
    assert!(
        matches!(&*ability.effect, Effect::TargetOnly { .. }),
        "outer effect must be TargetOnly, got {:?}",
        ability.effect
    );

    // SequentialSibling sub-ability: CreateDelayedTrigger.
    let sub = ability
        .sub_ability
        .as_ref()
        .expect("Hunter's Insight has a sub-ability");
    let Effect::CreateDelayedTrigger { condition, .. } = &*sub.effect else {
        panic!(
            "sub-ability effect must be CreateDelayedTrigger, got {:?}",
            sub.effect
        );
    };

    // The delayed trigger condition is a WheneverEvent over a DamageDone trigger.
    let DelayedTriggerCondition::WheneverEvent { trigger } = condition else {
        panic!("delayed trigger condition must be WheneverEvent, got {condition:?}");
    };

    // valid_source binds to the chosen creature (the outer TargetOnly target).
    assert_eq!(
        trigger.valid_source,
        Some(TargetFilter::ParentTarget),
        "valid_source must be ParentTarget — binds to the chosen creature (#384)"
    );

    // valid_target covers both players and planeswalkers.
    assert_eq!(
        trigger.valid_target,
        Some(TargetFilter::Or {
            filters: vec![
                TargetFilter::Player,
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Planeswalker],
                    controller: None,
                    properties: vec![],
                }),
            ],
        }),
        "valid_target must be Or[Player, Planeswalker] — covers the planeswalker clause"
    );

    // The draw is mandatory — the Oracle text has no "you may".
    assert!(
        !trigger.optional,
        "the delayed trigger is mandatory (optional == false)"
    );
}

/// Case C — a non-targeted decoy creature deals combat damage. The delayed
/// trigger's `valid_source: ParentTarget` does not match the decoy, so no draw
/// occurs — but the decoy still deals its combat damage, proving combat
/// happened (distinguishing "trigger correctly declined" from "no combat").
#[test]
fn non_targeted_attacker_does_not_trigger_draw() {
    let (mut runner, hunters_insight, insight_bear) = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let insight_bear = scenario.add_creature(P0, "Insight Bear", 3, 3).id();
        // A SECOND creature — the decoy attacker, NOT the target of Hunter's Insight.
        let _decoy = scenario.add_creature(P0, "Decoy Bear", 2, 2).id();
        let hunters_insight = scenario
            .add_spell_to_hand_from_oracle(P0, "Hunter's Insight", true, HUNTERS_INSIGHT_ORACLE)
            .id();
        for name in [
            "Library Card 1",
            "Library Card 2",
            "Library Card 3",
            "Library Card 4",
            "Library Card 5",
        ] {
            scenario.add_card_to_library_top(P0, name);
        }
        let runner = scenario.build();
        (runner, hunters_insight, insight_bear)
    };

    // Locate the decoy by name (its id was scoped to the block above).
    let decoy = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Decoy Bear")
        .expect("Decoy Bear on battlefield")
        .id;

    // Target Hunter's Insight at Insight Bear specifically (two legal creatures).
    cast_targeting(&mut runner, hunters_insight, insight_bear);

    let hand_before = hand_count(&runner, P0);
    let life_before_p1 = runner.life(P1);

    // Attack with the DECOY only — Insight Bear stays home.
    run_combat(&mut runner, vec![decoy], vec![]);
    // CR 510.3a: drain the stack so the assertion is meaningful even though no
    // Hunter's Insight trigger should match.
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_count(&runner, P0),
        hand_before,
        "valid_source: ParentTarget did not match the decoy — no draw (#384 guard)"
    );
    assert_eq!(
        runner.life(P1),
        life_before_p1 - 2,
        "CR 510.1b: the decoy's 2 combat damage still hit P1 — combat did happen"
    );
}
