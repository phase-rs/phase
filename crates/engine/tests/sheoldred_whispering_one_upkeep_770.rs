//! Regression coverage for issue #770.
//!
//! The reported symptom was that upkeep triggers, including Sheoldred,
//! Whispering One's graveyard-return trigger, did not activate. This drives the
//! real Oracle-to-runtime trigger path and requires an interactive target
//! selection so a one-target auto path cannot mask the bug.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SHEOLDRED_ORACLE: &str =
    "At the beginning of your upkeep, return target creature card from your graveyard to the battlefield.";

#[test]
fn sheoldred_whispering_one_upkeep_returns_chosen_creature_from_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_creature_from_oracle(P0, "Sheoldred, Whispering One", 6, 6, SHEOLDRED_ORACLE);
    let returned_id = scenario
        .add_creature_to_graveyard(P0, "Returned Creature", 2, 2)
        .id();
    let left_id = scenario
        .add_creature_to_graveyard(P0, "Left Creature", 3, 3)
        .id();

    let mut runner = scenario.build();
    runner.advance_to_upkeep();

    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            player,
            target_slots,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(runner.state().active_player, P0);
            assert_eq!(runner.state().phase, Phase::Upkeep);

            let legal_targets = &target_slots
                .first()
                .expect("Sheoldred trigger must have a target slot")
                .legal_targets;
            assert!(
                legal_targets.contains(&TargetRef::Object(returned_id)),
                "first slot must include the returned creature card"
            );
            assert!(
                legal_targets.contains(&TargetRef::Object(left_id)),
                "first slot must include the other legal graveyard creature card"
            );
        }
        other => {
            panic!("expected TriggerTargetSelection for Sheoldred upkeep trigger, got {other:?}")
        }
    }

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(returned_id)),
        })
        .expect("choosing a legal graveyard creature target must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&returned_id].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&left_id].zone, Zone::Graveyard);
}
