//! Issue #1079: Cityscape Leveler — "When you cast this spell and whenever this creature
//! attacks, destroy up to one target nonland permanent. Its controller creates a tapped
//! Powerstone token." The token is created while the trigger resolves (CR 608.2c), under the
//! destroyed permanent's controller (CR 111.2).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CITYSCAPE_LEVELER: &str = "Trample\nWhen you cast this spell and whenever this creature attacks, destroy up to one target nonland permanent. Its controller creates a tapped Powerstone token.\nUnearth {8}";

fn powerstones(runner: &GameRunner) -> Vec<(PlayerId, bool)> {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.zone == Zone::Battlefield && obj.name == "Powerstone")
        .map(|obj| (obj.controller, obj.tapped))
        .collect()
}

fn opponent_creature(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_creature(P1, "Opposing Bear", 2, 2).id()
}

/// The cast trigger resolves above the Leveler spell: the opponent's creature is destroyed
/// and the opponent gets the tapped Powerstone then, before the Leveler itself resolves.
#[test]
fn cast_trigger_gives_the_destroyed_permanents_controller_a_tapped_powerstone_at_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = opponent_creature(&mut scenario);
    let leveler = scenario
        .add_creature_to_hand(P0, "Cityscape Leveler", 8, 8)
        .from_oracle_text_with_keywords(&["Trample", "Unearth"], CITYSCAPE_LEVELER)
        .id();
    let mut runner = scenario.build();

    runner.cast(leveler).target_objects(&[bear]).commit();
    assert_eq!(
        runner.state().stack.len(),
        2,
        "the cast trigger goes on the stack above the Leveler"
    );
    runner.resolve_top();

    assert_eq!(runner.state().objects[&bear].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&leveler].zone,
        Zone::Stack,
        "reach: the Leveler hasn't resolved yet"
    );
    assert_eq!(
        powerstones(&runner),
        vec![(P1, true)],
        "CR 608.2c: the tapped Powerstone is created during the trigger's resolution, \
         under the destroyed permanent's controller"
    );
}

/// The attack trigger: same rider, same controller.
#[test]
fn attack_trigger_gives_the_destroyed_permanents_controller_a_tapped_powerstone() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = opponent_creature(&mut scenario);
    let leveler = scenario
        .add_creature(P0, "Cityscape Leveler", 8, 8)
        .from_oracle_text_with_keywords(&["Trample", "Unearth"], CITYSCAPE_LEVELER)
        .id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(leveler, AttackTarget::Player(P1))])
        .expect("the Leveler can attack");
    let WaitingFor::TriggerTargetSelection { player: P0, .. } = runner.state().waiting_for else {
        panic!(
            "the attack trigger asks for its target, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(bear)],
        })
        .expect("the Bear is a legal target");
    runner.resolve_top();

    assert_eq!(runner.state().objects[&bear].zone, Zone::Graveyard);
    assert_eq!(powerstones(&runner), vec![(P1, true)]);
}
