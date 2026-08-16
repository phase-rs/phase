//! Runtime regression for The Skullspore Nexus's dynamic-P/T dies-trigger token.
//!
//! "Whenever one or more nontoken creatures you control die, create a green
//! Fungus Dinosaur creature token with base power and toughness each equal to
//! the total power of those creatures."
//!
//! These tests drive the real cast/SBA/trigger pipeline (`GameRunner::cast(..)
//! .resolve()`), not the parser. The token's base P/T is a creation-time
//! snapshot of the total power of the creatures in the TRIGGERING BATCH
//! (`QuantityRef::TrackedSetAggregate { source: TriggeringBatch }`), read from
//! `state.current_trigger_events` at resolution — CR 603.2c + CR 603.10a.
//!
//! Revert-to-red anchors (each asserted below):
//! - Without the `source`/Part-B parser fix, the token lowers to `Unimplemented`
//!   or a 0/0 `Variable` token — the P/T assertions fail.
//! - Under the rejected `ZoneChangeAggregateThisTurn` alternative, the
//!   batch-precision test's second token reads 8/8 instead of 5/5.
//! - A live `Aggregate { creatures you control }` misparse would exclude the
//!   dead batch (they left the battlefield) and/or include the living survivor —
//!   the non-contamination assertions fail.

use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const NEXUS_ORACLE: &str = "This spell costs {X} less to cast, where X is the greatest power among creatures you control.\nWhenever one or more nontoken creatures you control die, create a green Fungus Dinosaur creature token with base power and toughness each equal to the total power of those creatures.\n{2}, {T}: Double target creature's power until end of turn.";

const PYROCLASM_ORACLE: &str = "Pyroclasm deals 2 damage to each creature.";
const MURDER_ORACLE: &str = "Destroy target creature.";

/// Every Fungus Dinosaur token controlled by `player` on the battlefield, as
/// `(power, toughness)` pairs.
fn dinosaur_tokens(
    state: &engine::types::game_state::GameState,
    player: engine::types::player::PlayerId,
) -> Vec<(Option<i32>, Option<i32>)> {
    state
        .objects
        .values()
        .filter(|o| {
            o.is_token
                && o.zone == Zone::Battlefield
                && o.controller == player
                && o.card_types.subtypes.iter().any(|s| s == "Dinosaur")
        })
        .map(|o| (o.power, o.toughness))
        .collect()
}

#[test]
fn skullspore_token_pt_equals_died_batch_total_power() {
    // CR 603.2c + CR 603.10a: two nontoken creatures (power 3 and 5) die in one
    // batch; the token's base P/T is their total power (8), read via LKI at
    // death time. A living third creature (power 9) must NOT contribute.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Non-creature artifact so a "deals damage to each creature" wipe never
    // kills the Nexus and pollutes its own trigger batch.
    let _nexus = scenario
        .add_creature_from_oracle(P0, "The Skullspore Nexus", 0, 0, NEXUS_ORACLE)
        .as_artifact()
        .id();

    // Damage (not -X/-X) is the kill tool: it does not lower power, so each
    // victim contributes its full power to the death-time snapshot.
    let victim_a = scenario.add_creature(P0, "Victim A", 3, 2).id();
    let victim_b = scenario.add_creature(P0, "Victim B", 5, 2).id();
    let survivor = scenario.add_creature(P0, "Survivor", 9, 9).id();

    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.cast(pyroclasm).resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();

    // Reach-guard: the two victims actually died and the survivor lived, so the
    // trigger reached the real batched-dies branch (not a vacuous pass).
    assert_eq!(state.objects[&victim_a].zone, Zone::Graveyard);
    assert_eq!(state.objects[&victim_b].zone, Zone::Graveyard);
    assert_eq!(state.objects[&survivor].zone, Zone::Battlefield);

    let tokens = dinosaur_tokens(state, P0);
    assert_eq!(
        tokens.len(),
        1,
        "exactly one Fungus Dinosaur token, got {tokens:?}"
    );
    // Sum of the DIED batch (3 + 5), not the living survivor (9), not 0.
    assert_eq!(
        tokens[0],
        (Some(8), Some(8)),
        "token base P/T must equal the died batch's total power (3+5=8)"
    );
}

#[test]
fn skullspore_batch_precision_second_death_reads_only_its_own_batch() {
    // Discriminates TriggeringBatch from the rejected ZoneChangeAggregateThisTurn:
    // two SEPARATE single-creature death events in one turn must each read ONLY
    // their own batch. ZoneChangeAggregateThisTurn would make the second token
    // 8/8 (both died this turn); TriggeringBatch keeps it 5/5.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _nexus = scenario
        .add_creature_from_oracle(P0, "The Skullspore Nexus", 0, 0, NEXUS_ORACLE)
        .as_artifact()
        .id();

    let victim_a = scenario.add_creature(P0, "Victim A", 3, 3).id();
    let victim_b = scenario.add_creature(P0, "Victim B", 5, 3).id();

    let murder_a = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let murder_b = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    // First death event: only Victim A (power 3).
    runner.cast(murder_a).target_objects(&[victim_a]).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&victim_a].zone, Zone::Graveyard);

    // Second, separate death event in the same turn: only Victim B (power 5).
    runner.cast(murder_b).target_objects(&[victim_b]).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&victim_b].zone, Zone::Graveyard);

    let mut tokens = dinosaur_tokens(runner.state(), P0);
    tokens.sort();
    assert_eq!(
        tokens,
        vec![(Some(3), Some(3)), (Some(5), Some(5))],
        "each token reads only its own death batch (3/3 then 5/5, never 8/8)"
    );
}

#[test]
fn skullspore_token_pt_is_creation_time_snapshot() {
    // CR 208.4b: the token's base P/T is fixed once at creation. After creation
    // the batch context (`current_trigger_events`) is gone and the died
    // creatures are in the graveyard, so a live re-read would yield 0; a snapshot
    // stays 8. Bumping an unrelated living creature's power must not move it.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _nexus = scenario
        .add_creature_from_oracle(P0, "The Skullspore Nexus", 0, 0, NEXUS_ORACLE)
        .as_artifact()
        .id();

    let victim_a = scenario.add_creature(P0, "Victim A", 3, 2).id();
    let _victim_b = scenario.add_creature(P0, "Victim B", 5, 2).id();
    let survivor = scenario.add_creature(P0, "Survivor", 9, 9).id();

    let pyroclasm = scenario
        .add_spell_to_hand_from_oracle(P0, "Pyroclasm", false, PYROCLASM_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.cast(pyroclasm).resolve();
    runner.advance_until_stack_empty();

    // Snapshot captured (concrete, not a deferred formula).
    let before = dinosaur_tokens(runner.state(), P0);
    assert_eq!(before, vec![(Some(8), Some(8))], "snapshot must be 8/8");

    // Mutate an unrelated living creature's power far upward; a live aggregate
    // would move, a snapshot will not.
    runner.state_mut().objects.get_mut(&survivor).unwrap().power = Some(100);

    let after = dinosaur_tokens(runner.state(), P0);
    assert_eq!(
        after,
        vec![(Some(8), Some(8))],
        "creation-time snapshot must not track later board changes (still 8/8)"
    );

    // Sanity: the batch source is truly gone after resolution — proves the 8/8
    // is a stored snapshot, not a live read that happened to be re-satisfied.
    assert!(
        runner.state().current_trigger_events.is_empty(),
        "trigger batch context must be cleared after resolution"
    );
    // Keep the victim binding meaningful (reach-guard, not vacuous).
    assert_eq!(runner.state().objects[&victim_a].zone, Zone::Graveyard);
}
