//! Issue #4269 — Nested Shambler must create N tokens where N is its buffed
//! power at the moment it dies, not its printed base power.
//!
//! Root cause: on battlefield exit `change_zone` snapshots the buffed power into
//! `lki_cache`, then `revert_layered_characteristics_to_base` reverts the live
//! graveyard object's P/T to base BEFORE the dies trigger resolves. The runtime
//! quantity resolver (`resolve_object_pt`, `ObjectScope::Source`) used to read
//! the live graveyard power without a zone guard, so the stale base value won
//! and the token count was 1 instead of N.
//!
//! CR 608.2h + CR 603.10a: a leaves-the-battlefield / dies trigger looks back at
//! the source as it last existed on the battlefield, so the buffed last-known
//! power must drive the token count.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::zones::Zone;

const NESTED_SHAMBLER: &str = "When this creature dies, create X tapped 1/1 green Squirrel creature tokens, where X is this creature's power.";
const NESTED_SHAMBLER_TOUGHNESS: &str = "When this creature dies, create X tapped 1/1 green Squirrel creature tokens, where X is this creature's toughness.";

fn drain_to_priority(runner: &mut GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 256,
            "drain exceeded bound; waiting_for = {:?}",
            runner.state().waiting_for
        );
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

/// Apply `n` +1/+1 (or -1/-1 when `n` is negative) counters and recompute the
/// continuous layers so the live (layer-7d) P/T reflects them. This drives the
/// real buff path: on death `change_zone` snapshots the counter-buffed `power`
/// into LKI, zone-exit cleanup removes the counters (CR 122.2), and
/// `revert_layered_characteristics_to_base` reverts the live object to base.
fn apply_pt_counters(runner: &mut GameRunner, id: ObjectId, n: i32) {
    let counter = if n >= 0 {
        CounterType::Plus1Plus1
    } else {
        CounterType::Minus1Minus1
    };
    *runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .counters
        .entry(counter)
        .or_insert(0) += n.unsigned_abs();
    evaluate_layers(runner.state_mut());
}

fn destroy_with_lethal_damage(runner: &mut GameRunner, id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .damage_marked = 99;

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_to_priority(runner);
}

/// Count tapped 1/1 green Squirrel creature tokens controlled by `player`.
fn squirrel_token_count(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token
                && o.zone == Zone::Battlefield
                && o.controller == player
                && o.card_types.core_types.contains(&CoreType::Creature)
                && o.card_types.subtypes.iter().any(|s| s == "Squirrel")
        })
        .collect::<Vec<_>>()
        .len()
}

#[test]
fn buffed_nested_shambler_creates_power_many_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let shambler = scenario
        .add_creature_from_oracle(P0, "Nested Shambler", 1, 1, NESTED_SHAMBLER)
        .id();
    let mut runner = scenario.build();

    // Two +1/+1 counters → 3/3 on the battlefield.
    apply_pt_counters(&mut runner, shambler, 2);
    destroy_with_lethal_damage(&mut runner, shambler);

    assert_eq!(
        squirrel_token_count(&runner, P0),
        3,
        "buffed (3/3) Nested Shambler must create 3 Squirrel tokens (X = buffed power), not 1"
    );
}

#[test]
fn unbuffed_nested_shambler_creates_one_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let shambler = scenario
        .add_creature_from_oracle(P0, "Nested Shambler", 1, 1, NESTED_SHAMBLER)
        .id();
    let mut runner = scenario.build();

    destroy_with_lethal_damage(&mut runner, shambler);

    assert_eq!(
        squirrel_token_count(&runner, P0),
        1,
        "unbuffed (1/1) Nested Shambler must create exactly 1 Squirrel token"
    );
}

#[test]
fn buffed_toughness_sibling_creates_toughness_many_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let shambler = scenario
        .add_creature_from_oracle(P0, "Toughness Shambler", 1, 1, NESTED_SHAMBLER_TOUGHNESS)
        .id();
    let mut runner = scenario.build();

    // Three +1/+1 counters → 4/4. X = toughness = 4.
    apply_pt_counters(&mut runner, shambler, 3);
    destroy_with_lethal_damage(&mut runner, shambler);

    assert_eq!(
        squirrel_token_count(&runner, P0),
        4,
        "toughness sibling: buffed (4/4) creature must create 4 tokens (X = buffed toughness)"
    );
}

#[test]
fn reduced_power_nested_shambler_creates_reduced_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    // Printed 4/4, reduced by two -1/-1 counters to 2/2 before death. LKI tracks
    // the modified (reduced) value, not the printed base, so the count is 2.
    let shambler = scenario
        .add_creature_from_oracle(P0, "Nested Shambler", 4, 4, NESTED_SHAMBLER)
        .id();
    let mut runner = scenario.build();

    apply_pt_counters(&mut runner, shambler, -2);
    destroy_with_lethal_damage(&mut runner, shambler);

    assert_eq!(
        squirrel_token_count(&runner, P0),
        2,
        "a creature whose power was reduced to 2 before dying must create 2 tokens (LKI tracks modified, not base)"
    );
}

#[test]
fn two_buffed_shamblers_dying_together_count_independently() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let a = scenario
        .add_creature_from_oracle(P0, "Nested Shambler", 1, 1, NESTED_SHAMBLER)
        .id();
    let b = scenario
        .add_creature_from_oracle(P0, "Nested Shambler", 1, 1, NESTED_SHAMBLER)
        .id();
    let mut runner = scenario.build();

    apply_pt_counters(&mut runner, a, 2); // 1/1 + 2 = 3/3 → 3 tokens
    apply_pt_counters(&mut runner, b, 4); // 1/1 + 4 = 5/5 → 5 tokens

    // Both die in the same SBA sweep.
    runner
        .state_mut()
        .objects
        .get_mut(&a)
        .unwrap()
        .damage_marked = 99;
    runner
        .state_mut()
        .objects
        .get_mut(&b)
        .unwrap()
        .damage_marked = 99;
    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_to_priority(&mut runner);

    assert_eq!(
        squirrel_token_count(&runner, P0),
        8,
        "two shamblers (3/3 and 5/5) dying together must create 3 + 5 = 8 tokens, no cross-contamination"
    );
}

#[test]
fn nested_shambler_zero_power_creates_no_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    // A 0/1 Nested Shambler (e.g. reduced to 0 power). X = 0 → no tokens, no panic.
    let shambler = scenario
        .add_creature_from_oracle(P0, "Nested Shambler", 0, 1, NESTED_SHAMBLER)
        .id();
    let mut runner = scenario.build();

    // Power stays 0; X = 0 → no tokens.
    destroy_with_lethal_damage(&mut runner, shambler);

    assert_eq!(
        squirrel_token_count(&runner, P0),
        0,
        "a 0-power Nested Shambler must create no tokens and must not panic"
    );
}
