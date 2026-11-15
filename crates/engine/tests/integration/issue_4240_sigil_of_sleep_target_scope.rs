//! Regression for issue #4240: Sigil of Sleep must create only its printed
//! creature target when the enchanted creature damages a player.
//!
//! CR 303.4b + CR 603.2 + CR 115.1d: an Aura's enchanted creature deals the
//! triggering damage; the triggered ability then receives its single printed
//! target as it is put on the stack.

use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

const SIGIL_OF_SLEEP_ORACLE: &str = "Whenever enchanted creature deals damage to a player, return target creature that player controls to its owner's hand.";

#[test]
fn sigil_of_sleep_uses_damaged_player_for_its_single_creature_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let enchanted_creature = scenario.add_creature(P0, "Sigil Bearer", 2, 2).id();
    let sigil = scenario
        .add_creature(P0, "Sigil of Sleep", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(SIGIL_OF_SLEEP_ORACLE)
        .id();
    let damaged_players_creature = scenario
        .add_creature(P1, "Damaged Player's Creature", 2, 2)
        .id();
    let second_damaged_creature = scenario
        .add_creature(P1, "Damaged Player's Second Creature", 3, 3)
        .id();
    let controller_creature = scenario
        .add_creature(P0, "Controller's Creature", 2, 2)
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), sigil, enchanted_creature);
    evaluate_layers(runner.state_mut());

    run_combat(&mut runner, vec![enchanted_creature], vec![]);

    for _ in 0..30 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must advance Sigil's trigger");
            }
            WaitingFor::TriggerTargetSelection { .. } => break,
            ref other => {
                panic!("Sigil's combat-damage trigger must reach target selection, got {other:?}")
            }
        }
    }

    let target_slots = match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => target_slots,
        other => panic!("Sigil's combat-damage trigger must reach target selection, got {other:?}"),
    };
    assert_eq!(
        target_slots.len(),
        1,
        "Sigil must create one creature target slot, not a phantom player slot"
    );
    assert_eq!(
        target_slots[0].legal_targets,
        vec![
            TargetRef::Object(damaged_players_creature),
            TargetRef::Object(second_damaged_creature),
        ],
        "every legal target must be a creature controlled by the damaged player"
    );
    assert!(
        !target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(controller_creature)),
        "the Aura controller's creature must not be legal for 'that player controls'"
    );

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(damaged_players_creature)],
        })
        .expect("choosing Sigil's only legal target must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&damaged_players_creature].zone,
        Zone::Hand,
        "Sigil must return the damaged player's chosen creature to its owner's hand"
    );
}
