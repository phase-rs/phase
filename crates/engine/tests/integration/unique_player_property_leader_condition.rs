//! U1 — the unified player-property selector and the "a player has more
//! `<property>` than each other player" unique-leader existential condition
//! (CR 603.4 + CR 102.1).
//!
//! Discriminates via a SYNTHETIC ability, not the card-driven `GiveControl`
//! observable, per PLAN-v3 §5.0: `unique_recipient_from_filter`
//! (`game/effects/gain_control.rs`) fails CLOSED on ANY tie ("ambiguous
//! GiveControl recipient"), so on Ghazbán Ogre/Wild Dogs/Sokenzan Renegade
//! "the condition blocked the trigger" and "the condition was silently
//! dropped" are the SAME observable (no control move either way). This test
//! instead gates a `GainLife` effect, whose life-delta observable does not
//! go through that ambiguity path at all.
//!
//! The condition MUST be parser-derived (S1-v2): `TriggerDefinition.condition`
//! is `Option<TriggerCondition>`, not `Option<StaticCondition>`, and the
//! bridge between them (`oracle_trigger.rs::static_condition_to_trigger_condition`)
//! is production code this test must ride, not reimplement.
//!
//! HAND-CONSTRUCTING the `TriggerCondition` is FORBIDDEN — it would sever
//! this test from the parser (U1's production) and every row below would
//! PASS ON REVERT, reintroducing the round-1 blocker (B1).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Comparator, Effect, QuantityExpr, TargetFilter,
    TriggerCondition, TriggerDefinition,
};
use engine::types::phase::Phase;
use engine::types::PlayerId;

/// Verbatim upkeep-trigger line (Ghazbán Ogre, Wild Dogs) — CR 119.3 axis.
const LIFE_AXIS_LINE: &str = "At the beginning of your upkeep, if a player has more life than \
                               each other player, the player with the most life gains control \
                               of this creature.";

/// Verbatim upkeep-trigger line (Sokenzan Renegade) — CR 402.3 axis.
const HAND_AXIS_LINE: &str = "At the beginning of your upkeep, if a player has more cards in \
                               hand than each other player, the player who has the most cards \
                               in hand gains control of this creature.";

/// Lift the PARSED trigger (condition included) off the real pipeline and
/// replace only `execute` with the supplied observable ability. Everything
/// else — `condition` above all — is whatever the parser produced.
fn leader_condition_trigger(
    oracle_line: &str,
    card_name: &str,
    execute: AbilityDefinition,
) -> TriggerDefinition {
    let parsed = parse_oracle_text(oracle_line, card_name, &[], &["Creature".to_string()], &[]);
    let mut trigger = parsed.triggers.into_iter().next().unwrap_or_else(|| {
        panic!("{card_name}'s upkeep trigger must parse into a TriggerDefinition")
    });
    // HAND-CONSTRUCTING the TriggerCondition here is FORBIDDEN: it would
    // sever this test from the parser and void its discrimination (S1-v2).
    // A `None` below means U1 did not bind at all, and without this
    // assertion every negative row in this file would go vacuously green.
    assert!(
        trigger.condition.is_some(),
        "{card_name}'s upkeep trigger must carry a parsed condition; a None \
         here means U1's `parse_unique_property_lead_tail` did not bind and \
         every negative row in this file would pass vacuously"
    );
    trigger.execute = Some(Box::new(execute));
    trigger
}

fn gain_life_3() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
    )
}

/// U1-R7's naive-`GE` guard, folded into fixture construction: assert the
/// LIFTED condition's shape is `EQ 1`, not `GE 1` — a `GE 1` slip is true
/// whenever anyone is at the max (i.e. always) and would not be revert
/// discriminating.
fn assert_unique_leader_shape(trigger: &TriggerDefinition) {
    let Some(TriggerCondition::QuantityComparison {
        comparator, rhs, ..
    }) = &trigger.condition
    else {
        panic!(
            "expected a QuantityComparison condition, got {:?}",
            trigger.condition
        );
    };
    assert_eq!(
        *comparator,
        Comparator::EQ,
        "the unique-leader existential must compare PlayerCount(..) EQ 1, not GE 1 \
         (a GE 1 slip is true whenever ANYONE is at the max, i.e. always)"
    );
    assert_eq!(
        *rhs,
        QuantityExpr::Fixed { value: 1 },
        "the unique-leader existential's RHS must be the fixed threshold 1"
    );
}

fn upkeep_gain_life_scenario(
    player_count: u8,
    seed: u64,
    trigger: TriggerDefinition,
) -> GameScenario {
    let mut scenario = GameScenario::new_n_player(player_count, seed);
    scenario.at_phase(Phase::Untap);
    scenario
        .add_creature(P0, "Leader-Condition Test Creature", 2, 2)
        .with_trigger_definition(trigger);
    scenario
}

/// U1-R0 — positive reach-guard: an IDENTICAL ability whose `condition` is
/// erased (not hand-built — simply removed) must still fire. Without this
/// row, every negative row below could pass vacuously through a harness that
/// never reaches the trigger at all.
#[test]
fn u1_r0_positive_reach_guard_unconditional_fires() {
    let mut trigger = leader_condition_trigger(LIFE_AXIS_LINE, "Ghazbán Ogre", gain_life_3());
    trigger.condition = None;
    let mut scenario = upkeep_gain_life_scenario(3, 1, trigger);
    scenario
        .with_life(P0, 20)
        .with_life(P1, 10)
        .with_life(PlayerId(2), 10);
    let mut runner = scenario.build();
    let before = runner.life(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        before + 3,
        "unconditional ability must fire"
    );
}

/// U1-R1 — the unique leader IS the controller. Passes either way (not a
/// discriminator by itself); pins the condition does not block a true case.
#[test]
fn u1_r1_unique_leader_is_controller_fires() {
    let trigger = leader_condition_trigger(LIFE_AXIS_LINE, "Ghazbán Ogre", gain_life_3());
    assert_unique_leader_shape(&trigger);
    let mut scenario = upkeep_gain_life_scenario(3, 2, trigger);
    scenario
        .with_life(P0, 20)
        .with_life(P1, 10)
        .with_life(PlayerId(2), 10);
    let mut runner = scenario.build();
    let before = runner.life(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), before + 3);
}

/// U1-R2 — LOAD-BEARING: a 2-way tie at the max is NOT "more than each
/// other player". REVERT-FAIL: with U1 reverted the condition is `None`, the
/// trigger fires unconditionally, and `life_delta(P0) == +3 != 0`.
#[test]
fn u1_r2_two_way_tie_blocks_trigger() {
    let trigger = leader_condition_trigger(LIFE_AXIS_LINE, "Ghazbán Ogre", gain_life_3());
    let mut scenario = upkeep_gain_life_scenario(3, 3, trigger);
    scenario
        .with_life(P0, 20)
        .with_life(P1, 20)
        .with_life(PlayerId(2), 10);
    let mut runner = scenario.build();
    let before = runner.life(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        before,
        "a 2-way tie at the max must NOT fire the trigger"
    );
}

/// U1-R3 — the existential is genuinely existential: the leader need not be
/// the controller. Does NOT fail on a plain revert (stated, not a
/// discriminator by itself) but does fail against a hypothetical hardcoded
/// `Controller` anchor.
#[test]
fn u1_r3_leader_not_controller_still_fires() {
    let trigger = leader_condition_trigger(LIFE_AXIS_LINE, "Ghazbán Ogre", gain_life_3());
    let mut scenario = upkeep_gain_life_scenario(3, 4, trigger);
    scenario
        .with_life(P0, 10)
        .with_life(P1, 20)
        .with_life(PlayerId(2), 10);
    let mut runner = scenario.build();
    let before = runner.life(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        before + 3,
        "the leader need not be the controller"
    );
}

/// U1-R4 — a 3-way tie. REVERT-FAIL (`+3 != 0`).
#[test]
fn u1_r4_three_way_tie_blocks_trigger() {
    let trigger = leader_condition_trigger(LIFE_AXIS_LINE, "Ghazbán Ogre", gain_life_3());
    let mut scenario = upkeep_gain_life_scenario(3, 5, trigger);
    scenario
        .with_life(P0, 15)
        .with_life(P1, 15)
        .with_life(PlayerId(2), 15);
    let mut runner = scenario.build();
    let before = runner.life(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        before,
        "a 3-way tie must NOT fire the trigger"
    );
}

/// U1-R5 — 2-PLAYER MINIMUM: the tie rule holds where "each other player" is
/// a single player. REVERT-FAIL.
#[test]
fn u1_r5_two_player_tie_blocks_trigger() {
    let trigger = leader_condition_trigger(LIFE_AXIS_LINE, "Ghazbán Ogre", gain_life_3());
    let mut scenario = upkeep_gain_life_scenario(2, 6, trigger);
    scenario.with_life(P0, 20).with_life(P1, 20);
    let mut runner = scenario.build();
    let before = runner.life(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        before,
        "a 2-player tie must NOT fire the trigger"
    );
}

/// U1-R6 — the `HandSize` axis end-to-end (Sokenzan Renegade's condition
/// shape). Two sub-cases: a clear leader fires; then, with the tie
/// introduced, the trigger stays blocked. REVERT-FAIL on the second
/// sub-case (`+3 != 0`).
#[test]
fn u1_r6_hand_size_axis_end_to_end() {
    // Sub-case A: hands 3/1/1 — P0 uniquely leads.
    let trigger_a = leader_condition_trigger(HAND_AXIS_LINE, "Sokenzan Renegade", gain_life_3());
    assert_unique_leader_shape(&trigger_a);
    let mut scenario_a = upkeep_gain_life_scenario(3, 7, trigger_a);
    scenario_a
        .with_cards_in_hand(P0, &["Card A1", "Card A2", "Card A3"])
        .with_cards_in_hand(P1, &["Card B1"])
        .with_cards_in_hand(PlayerId(2), &["Card C1"]);
    let mut runner_a = scenario_a.build();
    let before_a = runner_a.life(P0);
    runner_a.advance_to_upkeep();
    runner_a.advance_until_stack_empty();
    assert_eq!(
        runner_a.life(P0),
        before_a + 3,
        "a unique hand-size leader must fire"
    );

    // Sub-case B: hands 3/3/1 — a tie at the top. REVERT-FAIL.
    let trigger_b = leader_condition_trigger(HAND_AXIS_LINE, "Sokenzan Renegade", gain_life_3());
    let mut scenario_b = upkeep_gain_life_scenario(3, 8, trigger_b);
    scenario_b
        .with_cards_in_hand(P0, &["Card A1", "Card A2", "Card A3"])
        .with_cards_in_hand(P1, &["Card B1", "Card B2", "Card B3"])
        .with_cards_in_hand(PlayerId(2), &["Card C1"]);
    let mut runner_b = scenario_b.build();
    let before_b = runner_b.life(P0);
    runner_b.advance_to_upkeep();
    runner_b.advance_until_stack_empty();
    assert_eq!(
        runner_b.life(P0),
        before_b,
        "a hand-size tie at the max must NOT fire"
    );
}

/// Hostile fixture — MULTI-AUTHORITY / ANCHOR: a 4-player board where two
/// NON-CONTROLLER players tie at the top and the controller is third. First
/// production branch reached: `resolve_player_count`'s candidate loop.
#[test]
fn hostile_multi_authority_non_controller_tie_blocks_trigger() {
    let trigger = leader_condition_trigger(LIFE_AXIS_LINE, "Ghazbán Ogre", gain_life_3());
    let mut scenario = upkeep_gain_life_scenario(4, 9, trigger);
    scenario
        .with_life(P0, 10) // controller, third
        .with_life(P1, 20) // tied leader
        .with_life(PlayerId(2), 20) // tied leader
        .with_life(PlayerId(3), 5);
    let mut runner = scenario.build();
    let before = runner.life(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        before,
        "a tie among non-controller players must still block the trigger"
    );
}

/// Hostile fixture — ELIMINATED PLAYER, LIFE AXIS: a player eliminated while
/// holding the highest RECORDED life must not count toward the population;
/// the live leader still wins.
///
/// WHAT THIS ROW ACTUALLY PINS — a DISJUNCTION, not this diff's life-axis
/// filter on its own. Four revert experiments against this fixture, measured:
///
/// 1. Revert only `resolve_per_team_life`'s two `!p.is_eliminated` filters →
///    GREEN. Measured further, via
///    `cargo test -p phase-engine --no-fail-fast` under that revert: every
///    target is green (integration 7076 passed, 0 failed) except ONE lib unit
///    test —
///    `game::quantity::tests::life_total_min_excludes_eliminated_player_from_population`.
///    That is the whole of this filter's regression coverage.
/// 2. Neutralize `resolve_player_count`'s candidate-side `!p.is_eliminated` →
///    GREEN.
/// 3. Delete `topology::shared_resource_members`'s `is_alive` branch → GREEN.
/// 4. Remove the team-life filters AND the `is_alive` branch together → RED
///    (P0's life reads 20, expected 23).
///
/// `shared_resource_members`'s `is_alive` branch is PRE-EXISTING at
/// upstream/main and untouched by this diff, so this fixture stays green with
/// this diff's entire life-axis production change reverted: it does NOT
/// discriminate that change on its own. It is an end-to-end guard that the
/// combined path keeps a departed player out of the population, not a
/// regression pin for the new filter. That pin is the `quantity.rs` unit test
/// named above; its doc block explains which read shapes the filter actually
/// changes.
///
/// The mirror HAND-axis fixture,
/// `f7_hostile_eliminated_player_hand_axis_leader_still_wins`
/// (`superlative_player_subject_control.rs`), DOES discriminate its own guard:
/// reverting `resolve_per_player_scalar`'s `AllPlayers` `!p.is_eliminated`
/// turns that row RED (measured). The two axes take separate resolution
/// paths — `QuantityRef::HandSize` through `resolve_per_player_scalar`,
/// `QuantityRef::LifeTotal` through `resolve_per_team_life`.
#[test]
fn hostile_eliminated_player_life_axis_excluded_from_population() {
    let trigger = leader_condition_trigger(LIFE_AXIS_LINE, "Ghazbán Ogre", gain_life_3());
    let mut scenario = upkeep_gain_life_scenario(3, 10, trigger);
    scenario
        .with_life(P0, 20) // controller, live unique leader
        .with_life(P1, 99) // eliminated, would otherwise "lead"
        .with_life(PlayerId(2), 10);
    let mut runner = scenario.build();
    runner.state_mut().players[1].is_eliminated = true;
    let before = runner.life(P0);
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        before + 3,
        "an eliminated player's life must not block the live leader's trigger"
    );
}
