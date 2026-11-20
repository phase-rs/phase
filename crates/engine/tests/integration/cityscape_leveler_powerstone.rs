//! Issue #1079: Cityscape Leveler — "When you cast this spell and whenever this creature
//! attacks, destroy up to one target nonland permanent. Its controller creates a tapped
//! Powerstone token." The destroyed permanent's controller, read by last known information
//! (CR 608.2h), creates the token while the trigger resolves (CR 608.2c), so it enters under
//! that player's control (CR 111.2).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const INVOKE_THE_WINDS: &str = "Gain control of target artifact or creature. Untap it.";

const CITYSCAPE_LEVELER: &str = "Trample\nWhen you cast this spell and whenever this creature attacks, destroy up to one target nonland permanent. Its controller creates a tapped Powerstone token.\nUnearth {8}";

/// The trigger has finished resolving: P0 has priority again and `stack` is what remains.
fn assert_trigger_done(runner: &GameRunner, stack: &[ObjectId]) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "the trigger has finished and P0 has priority again, got {:?}",
        runner.state().waiting_for
    );
    let remaining: Vec<ObjectId> = runner.state().stack.iter().map(|entry| entry.id).collect();
    assert_eq!(remaining, stack, "only the expected stack entries remain");
}

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
    scenario.add_creature(P1, "Grizzly Bears", 2, 2).id()
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
    assert!(
        powerstones(&runner).is_empty(),
        "no Powerstone exists before the trigger resolves"
    );
    runner.resolve_top();

    assert_eq!(runner.state().objects[&bear].zone, Zone::Graveyard);
    assert_trigger_done(&runner, &[leveler]);
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
    assert!(
        powerstones(&runner).is_empty(),
        "no Powerstone exists before the trigger resolves"
    );
    runner.resolve_top();

    assert_eq!(runner.state().objects[&bear].zone, Zone::Graveyard);
    assert_trigger_done(&runner, &[]);
    assert_eq!(powerstones(&runner), vec![(P1, true)]);
}

/// "Its controller" is the destroyed permanent's controller, not its owner: P0 steals P1's
/// Bear with Invoke the Winds, then the Leveler destroys it, and the Powerstone goes to P0.
#[test]
fn powerstone_goes_to_the_destroyed_permanents_controller_not_its_owner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = opponent_creature(&mut scenario);
    let steal = scenario
        .add_spell_to_hand_from_oracle(P0, "Invoke the Winds", false, INVOKE_THE_WINDS)
        .id();
    let leveler = scenario
        .add_creature_to_hand(P0, "Cityscape Leveler", 8, 8)
        .from_oracle_text_with_keywords(&["Trample", "Unearth"], CITYSCAPE_LEVELER)
        .id();
    let mut runner = scenario.build();

    runner.cast(steal).target_objects(&[bear]).resolve();
    let stolen = &runner.state().objects[&bear];
    assert_eq!(
        (stolen.controller, stolen.owner),
        (P0, P1),
        "reach: P0 controls P1's Bear"
    );

    runner.cast(leveler).target_objects(&[bear]).commit();
    runner.resolve_top();

    assert_eq!(runner.state().objects[&bear].zone, Zone::Graveyard);
    assert_trigger_done(&runner, &[leveler]);
    assert_eq!(
        powerstones(&runner),
        vec![(P0, true)],
        "CR 608.2h: the Bear's controller when it was destroyed creates the Powerstone"
    );
}
