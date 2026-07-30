//! Regression for GitHub issue #6442 — Elenda, the Dusk Rose must create
//! Vampire tokens equal to her power as she last existed on the battlefield
//! (counters included), not her printed power and not zero.
//!
//! Oracle: "Lifelink / Whenever another creature dies, put a +1/+1 counter on
//! Elenda. / When Elenda dies, create X 1/1 white Vampire creature tokens
//! with lifelink, where X is Elenda's power."
//!
//! CR 608.2h + CR 603.10a: a dies trigger's reference to its own source's
//! power looks back at the source as it last existed on the battlefield. This
//! is the same `ObjectScope::Source` look-back path fixed generically for
//! Nested Shambler (issue #4269 / `issue_4269_nested_shambler_death_tokens.rs`);
//! these tests pin the same behavior against Elenda's specific card text,
//! including the card's own printed ruling about dying simultaneously with
//! another creature.
//!
//! Per the card's ruling: "If Elenda dies at the same time as another
//! creature, both of its triggered abilities trigger. However, the first one
//! won't do anything since you can't put a +1/+1 counter on Elenda." — the
//! token count must still use her pre-death power, accumulated from *prior*
//! deaths, not the fizzled simultaneous one.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::zones::Zone;

const ELENDA: &str = "Lifelink\nWhenever another creature dies, put a +1/+1 counter on Elenda.\nWhen Elenda dies, create X 1/1 white Vampire creature tokens with lifelink, where X is Elenda's power.";

/// Drains priority passes, answering the `OrderTriggers` prompt (arbitrary
/// order — the two triggers are independent of each other, per the ruling)
/// so simultaneous multi-trigger batches don't stall the drain.
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
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(engine::types::actions::GameAction::OrderTriggers { order })
                    .unwrap();
            }
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

/// Apply `n` +1/+1 counters directly and recompute layers, mirroring
/// `issue_4269_nested_shambler_death_tokens.rs`'s helper of the same name.
fn apply_pt_counters(runner: &mut GameRunner, id: ObjectId, n: i32) {
    *runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .counters
        .entry(CounterType::Plus1Plus1)
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

fn vampire_token_count(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.is_token
                && o.zone == Zone::Battlefield
                && o.controller == player
                && o.card_types.core_types.contains(&CoreType::Creature)
                && o.card_types.subtypes.iter().any(|s| s == "Vampire")
        })
        .count()
}

#[test]
fn elenda_unbuffed_creates_one_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let elenda = scenario
        .add_creature_from_oracle(P0, "Elenda, the Dusk Rose", 1, 1, ELENDA)
        .id();
    let mut runner = scenario.build();

    destroy_with_lethal_damage(&mut runner, elenda);

    assert_eq!(
        vampire_token_count(&runner, P0),
        1,
        "an unbuffed (1/1) Elenda must create exactly 1 Vampire token"
    );
}

#[test]
fn elenda_buffed_by_counters_creates_power_many_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let elenda = scenario
        .add_creature_from_oracle(P0, "Elenda, the Dusk Rose", 1, 1, ELENDA)
        .id();
    let mut runner = scenario.build();

    apply_pt_counters(&mut runner, elenda, 2); // 3/3
    destroy_with_lethal_damage(&mut runner, elenda);

    assert_eq!(
        vampire_token_count(&runner, P0),
        3,
        "a buffed (3/3) Elenda must create 3 Vampire tokens (X = last-known power), not 1"
    );
}

/// Buffs Elenda via her OWN "whenever another creature dies" trigger,
/// resolving through the real stack (not a direct counter mutation), so the
/// counters that must feed the death trigger's LKI read are the product of
/// ordinary ability resolution.
#[test]
fn elenda_buffed_via_real_trigger_stack_creates_power_many_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let elenda = scenario
        .add_creature_from_oracle(P0, "Elenda, the Dusk Rose", 1, 1, ELENDA)
        .id();
    let victim_a = scenario.add_vanilla(P0, 1, 1);
    let victim_b = scenario.add_vanilla(P0, 1, 1);
    let mut runner = scenario.build();

    destroy_with_lethal_damage(&mut runner, victim_a);
    destroy_with_lethal_damage(&mut runner, victim_b);
    assert_eq!(
        runner
            .state()
            .objects
            .get(&elenda)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        2,
        "reach guard: two prior deaths must land two real +1/+1 counters on Elenda"
    );

    destroy_with_lethal_damage(&mut runner, elenda);

    assert_eq!(
        vampire_token_count(&runner, P0),
        3,
        "Elenda buffed to 3/3 via her own trigger must create 3 Vampire tokens"
    );
}

/// The card's own ruling: Elenda dying at the same time as another creature
/// still fires her "whenever another creature dies" trigger, but it fizzles
/// (she's no longer there to receive the counter). Her dies trigger must
/// still use her power as it stood immediately before this simultaneous
/// event — the counters from EARLIER deaths, not zero and not printed power.
#[test]
fn elenda_dying_simultaneously_with_another_creature_still_uses_prior_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let elenda = scenario
        .add_creature_from_oracle(P0, "Elenda, the Dusk Rose", 1, 1, ELENDA)
        .id();
    let victim = scenario.add_vanilla(P0, 1, 1);
    let mut runner = scenario.build();

    apply_pt_counters(&mut runner, elenda, 2); // 3/3, from prior deaths

    // Elenda and `victim` die in the SAME SBA sweep.
    runner
        .state_mut()
        .objects
        .get_mut(&elenda)
        .unwrap()
        .damage_marked = 99;
    runner
        .state_mut()
        .objects
        .get_mut(&victim)
        .unwrap()
        .damage_marked = 99;
    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_to_priority(&mut runner);

    assert_eq!(
        vampire_token_count(&runner, P0),
        3,
        "Elenda dying simultaneously with another creature must still create \
         3 tokens from her pre-death (3/3) power, per the card's own ruling"
    );
}
