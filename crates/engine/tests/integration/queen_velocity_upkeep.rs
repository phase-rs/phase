//! Real Oracle activation timing and actor permissions for `any upkeep step`.

use engine::game::restrictions::check_activation_restrictions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityKind, ActivationRestriction};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const DWARVEN_ARMORY: &str =
    "{2}, Sacrifice a land: Put a +2/+2 counter on target creature. Activate only during any upkeep step.";
const TOLARIA: &str = "{T}: Add {U}.\n{T}: Target creature loses banding and all \"bands with other\" abilities until end of turn. Activate only during any upkeep step.";

fn assert_any_upkeep_gate(
    runner: &mut GameRunner,
    source: ObjectId,
    ability_index: usize,
    restrictions: &[ActivationRestriction],
    card_name: &str,
) {
    for active_player in [P0, P1] {
        let state = runner.state_mut();
        state.active_player = active_player;
        state.phase = Phase::Upkeep;
        assert!(
            check_activation_restrictions(state, P0, source, ability_index, restrictions).is_ok(),
            "{card_name} must be legal during {active_player:?}'s upkeep"
        );
    }

    for phase in [Phase::PreCombatMain, Phase::End] {
        let state = runner.state_mut();
        state.phase = phase;
        assert!(
            check_activation_restrictions(state, P0, source, ability_index, restrictions).is_err(),
            "{card_name} must be illegal during {phase:?}"
        );
    }
}

#[test]
fn any_upkeep_activation_timing_allows_either_players_upkeep_only() {
    let mut armory_scenario = GameScenario::new();
    let armory = armory_scenario
        .add_enchantment_from_oracle(P0, "Dwarven Armory", DWARVEN_ARMORY)
        .id();
    let mut armory_runner = armory_scenario.build();
    let armory_restrictions = armory_runner.state().objects[&armory].abilities[0]
        .activation_restrictions
        .clone();
    assert_any_upkeep_gate(
        &mut armory_runner,
        armory,
        0,
        &armory_restrictions,
        "Dwarven Armory",
    );

    let mut tolaria_scenario = GameScenario::new();
    let tolaria = tolaria_scenario
        .add_land_from_oracle(P0, "Tolaria", TOLARIA)
        .id();
    let mut tolaria_runner = tolaria_scenario.build();
    let (tolaria_index, tolaria_restrictions) = tolaria_runner.state().objects[&tolaria]
        .abilities
        .iter()
        .enumerate()
        .find(|(_, ability)| !ability.activation_restrictions.is_empty())
        .map(|(index, ability)| (index, ability.activation_restrictions.clone()))
        .expect("Tolaria's restricted ability must survive Oracle parsing");
    assert_any_upkeep_gate(
        &mut tolaria_runner,
        tolaria,
        tolaria_index,
        &tolaria_restrictions,
        "Tolaria",
    );
}

// CR 602.1b + CR 602.2 + CR 602.5: an explicit any-player permission allows
// noncontroller activation, but the timing instruction still restricts it.
// CR 503.1: the upkeep step gives players a priority window for activation.
#[test]
fn opponent_can_activate_any_player_abilities_only_during_either_upkeep() {
    let cards = [
        (
            "Armageddon Clock",
            "At the beginning of your upkeep, put a doom counter on this artifact.\nAt the beginning of your draw step, this artifact deals damage equal to the number of doom counters on it to each player.\n{4}: Remove a doom counter from this artifact. Any player may activate this ability but only during any upkeep step.",
            CounterType::Generic("doom".into()),
            4,
        ),
        (
            "Infinite Hourglass",
            "At the beginning of your upkeep, put a time counter on this artifact.\nAll creatures get +1/+0 for each time counter on this artifact.\n{3}: Remove a time counter from this artifact. Any player may activate this ability but only during any upkeep step.",
            CounterType::Time,
            3,
        ),
    ];
    for (name, oracle, counter, mana) in cards {
        for active_player in [P0, P1] {
            for phase in [Phase::Upkeep, Phase::PreCombatMain, Phase::End] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(phase);
                let source = scenario.add_artifact_from_oracle(P0, name, oracle).id();
                scenario.with_counter(source, counter.clone(), 2);
                scenario.with_mana_pool(
                    P1,
                    (0..mana)
                        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                        .collect(),
                );
                let mut runner = scenario.build();
                runner.state_mut().active_player = active_player;
                runner.state_mut().priority_player = P1;
                runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
                let index = runner.state().objects[&source]
                    .abilities
                    .iter()
                    .position(|ability| ability.kind == AbilityKind::Activated)
                    .expect("the parsed card must have an activated ability");
                if phase == Phase::Upkeep {
                    runner.activate(source, index).resolve();
                    assert_eq!(
                        runner.state().objects[&source].counters.get(&counter),
                        Some(&1),
                        "P1 must remove a counter from {name} during {active_player:?}'s upkeep"
                    );
                } else {
                    assert!(
                        runner
                            .act(GameAction::ActivateAbility {
                                source_id: source,
                                ability_index: index,
                            })
                            .is_err(),
                        "{name} must reject P1's activation during {phase:?}"
                    );
                }
            }
        }
    }
}
