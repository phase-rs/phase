//! Phyrexian Fleshgorger's Ward payment reads its power when Ward resolves.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, ObjectScope, QuantityExpr, QuantityRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const FLESHGORGER: &str = "Menace, lifelink\nWard—Pay life equal to Phyrexian Fleshgorger's power.";

#[test]
fn fleshgorger_ward_prompts_for_its_power_at_resolution_and_charges_the_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 20);
    let fleshgorger = scenario
        .add_creature_from_oracle(P0, "Phyrexian Fleshgorger", 7, 5, FLESHGORGER)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner.cast(murder).target_objects(&[fleshgorger]).commit();

    // CR 702.21a + CR 608.2h: Ward reads the source's power as the trigger
    // resolves, not when the creature first became targeted.
    {
        let object = runner
            .state_mut()
            .objects
            .get_mut(&fleshgorger)
            .expect("Fleshgorger remains on the battlefield");
        object.base_power = Some(3);
        object.power = Some(3);
    }
    runner.advance_until_stack_empty();

    let WaitingFor::UnlessPayment { player, cost, .. } = &runner.state().waiting_for else {
        panic!(
            "Fleshgorger's Ward must prompt the opponent for its life payment, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1);
    assert!(matches!(
        cost,
        AbilityCost::PayLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Source,
                },
            },
        }
    ));

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the opponent pays Fleshgorger's Ward cost");
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        17,
        "Ward must charge the opponent Fleshgorger's current 3 power, not its original 7 power"
    );
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == murder),
        "paying Ward must leave the targeted spell on the stack"
    );
}
