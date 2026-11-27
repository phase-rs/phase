//! Ghired, Mirror of the Wilds: "Haste. Nontoken creatures you control have
//! "{T}: Create a token that's a copy of target token you control that entered
//! this turn.""
//!
//! Issue #716 reported that the granted ability targeted the player instead.
//! These rows drive the granted ability through the offered actions and pin its
//! target set, its resolution and which creatures get it.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::{ObjectId, TargetRef};

const GHIRED_NAME: &str = "Ghired, Mirror of the Wilds";
const GHIRED: &str = "Haste\nNontoken creatures you control have \"{T}: Create a token that's a copy of target token you control that entered this turn.\"";
const RAISE_THE_ALARM: &str = "Create two 1/1 white Soldier creature tokens.";
const RUNECLAW_BEAR: &str = "Runeclaw Bear";

fn raise_the_alarm(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM)
        .id()
}

fn soldier_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield
                && obj.controller == P0
                && obj.is_token
                && obj.name == "Soldier"
        })
        .map(|obj| obj.id)
        .collect()
}

fn activations(runner: &GameRunner) -> Vec<GameAction> {
    legal_actions(runner.state())
        .into_iter()
        .filter(|action| matches!(action, GameAction::ActivateAbility { .. }))
        .collect()
}

#[test]
fn ghired_granted_ability_targets_and_copies_only_an_eligible_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ghired_spell = scenario
        .add_creature_to_hand_from_oracle(P0, GHIRED_NAME, 3, 3, GHIRED)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let fresh_creature = scenario.add_creature_to_hand(P0, RUNECLAW_BEAR, 2, 2).id();
    let old_token = scenario.add_creature(P0, "Soldier", 1, 1).id();
    let opposing_token = scenario.add_creature(P1, "Soldier", 1, 1).id();
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    let alarm = raise_the_alarm(&mut scenario);

    let mut runner = scenario.build();
    let turn = runner.state().turn_number;
    runner
        .state_mut()
        .objects
        .get_mut(&old_token)
        .unwrap()
        .is_token = true;
    let opposing_soldier = runner.state_mut().objects.get_mut(&opposing_token).unwrap();
    opposing_soldier.is_token = true;
    opposing_soldier.entered_battlefield_turn = Some(turn);
    opposing_soldier.summoning_sick = true;

    runner.cast(ghired_spell).resolve();
    runner.cast(fresh_creature).resolve();
    assert_eq!(soldier_tokens(&runner), [old_token]);
    runner.cast(alarm).resolve();
    let soldiers: Vec<_> = soldier_tokens(&runner)
        .into_iter()
        .filter(|id| *id != old_token)
        .collect();
    let &[chosen, other] = soldiers.as_slice() else {
        panic!("Raise the Alarm must create two Soldiers, got {soldiers:?}");
    };

    let offers = activations(&runner);
    let activation = offers
        .iter()
        .find(|action| matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == bears))
        .expect("Grizzly Bears must offer Ghired's granted ability");
    runner.act(activation.clone()).unwrap();

    // CR 602.2b + CR 601.2c: The required target must be a token you control
    // that entered this turn, as specified by the granted ability.
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected target selection, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    let [slot] = target_slots.as_slice() else {
        panic!("expected one target slot, got {target_slots:?}");
    };
    assert!(!slot.optional);
    assert_eq!(slot.legal_targets.len(), 2, "{:?}", slot.legal_targets);
    assert!(slot.legal_targets.contains(&TargetRef::Object(chosen)));
    assert!(slot.legal_targets.contains(&TargetRef::Object(other)));

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(chosen)),
        })
        .unwrap();
    // CR 602.2: Activating the ability pays its tap cost.
    assert!(runner.state().objects[&bears].tapped);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        runner.state().stack[0].ability().unwrap().targets,
        [TargetRef::Object(chosen)]
    );
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    // CR 117.3b: The active player receives priority after resolution.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0, .. }
    ));

    // CR 111.1 + CR 111.2 + CR 707.2: Create one token under the activator's
    // control with the targeted Soldier's copiable characteristics.
    let tokens = soldier_tokens(&runner);
    assert_eq!(tokens.len(), 4);
    assert!(tokens.contains(&old_token));
    assert!(tokens.contains(&other));
    assert!(tokens.contains(&chosen));
    let copy = tokens
        .into_iter()
        .find(|id| *id != chosen && *id != other && *id != old_token)
        .unwrap();
    for id in [chosen, copy] {
        let soldier = &runner.state().objects[&id];
        assert_eq!((soldier.power, soldier.toughness), (Some(1), Some(1)));
        assert_eq!(soldier.color, [ManaColor::White]);
        assert!(!soldier.tapped);
    }

    let ghired = runner
        .state()
        .objects
        .values()
        .find(|obj| obj.zone == Zone::Battlefield && obj.name == GHIRED_NAME)
        .expect("Ghired must resolve to the battlefield");
    assert_eq!(ghired.entered_battlefield_turn, Some(turn));
    assert!(runner.state().objects.values().any(|obj| {
        obj.zone == Zone::Battlefield
            && obj.name == RUNECLAW_BEAR
            && obj.entered_battlefield_turn == Some(turn)
    }));
    // CR 613.1f: Only nontoken creatures you control receive the ability.
    // CR 302.6 + CR 702.10c: Ghired can pay the tap cost this turn via haste;
    // the newly cast Runeclaw Bear cannot. The older token is ready to tap.
    assert_eq!(offers.len(), 2, "{offers:?}");
    assert!(offers.iter().any(|action| matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == ghired.id)));

    // CR 117.3d: Check the opponent's options with the opponent holding priority.
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1, .. }
    ));
    assert!(activations(&runner).is_empty());
}

#[test]
fn ghired_granted_ability_requires_a_legal_target_to_be_offered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, GHIRED_NAME, 3, 3, GHIRED);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let alarm = raise_the_alarm(&mut scenario);
    let mut runner = scenario.build();

    // CR 602.2 + CR 602.2b + CR 601.2c: No legal activation can be
    // announced without the required target.
    assert!(activations(&runner).is_empty());
    runner.cast(alarm).resolve();
    assert!(activations(&runner).iter().any(|action| matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == bears)));
}
