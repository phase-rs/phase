//! Regression for Karona, False God's phase-triggered control handoff.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

const KARONA_ORACLE: &str = "Haste\n\
    At the beginning of each player's upkeep, that player untaps Karona and gains control of it.\n\
    Whenever Karona attacks, creatures of the creature type of your choice get +3/+3 until end of turn.";

/// CR 608.2c: P1 is the scoped player on P1's upkeep, so Karona's printed
/// untap and control instructions resolve for P1 in order — even though P0,
/// Karona's controller, controls the triggered ability.
#[test]
fn karona_upkeep_untaps_and_transfers_to_the_upkeep_player() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &player in &[P0, P1] {
        scenario.with_library_top(player, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    let karona = scenario
        .add_creature_from_oracle(P0, "Karona, False God", 5, 5, KARONA_ORACLE)
        .id();
    let mut runner = scenario.build();

    assert!(
        runner.state().objects[&karona]
            .trigger_definitions
            .iter_unchecked()
            .any(|entry| {
                entry.definition.mode == TriggerMode::Phase
                    && entry.definition.phase == Some(Phase::Upkeep)
            }),
        "complete Karona Oracle text must provide its phase/upkeep trigger"
    );
    // CR 502.3: P1's untap step untaps only permanents P1 controls, so a tapped
    // Karona under P0 stays tapped until its own trigger untaps it.
    // CR 508.1a: a tapped Karona also can't attack, so P0's combat declares no
    // attackers and the turn rolls forward on priority passes alone.
    runner.state_mut().objects.get_mut(&karona).unwrap().tapped = true;

    // The next upkeep after P0's precombat main is P1's.
    runner.advance_to_upkeep();
    assert!(
        runner.state().active_player == P1 && runner.state().phase == Phase::Upkeep,
        "must reach P1's upkeep; stopped at {:?} of {:?}'s turn on {:?}",
        runner.state().phase,
        runner.state().active_player,
        runner.state().waiting_for
    );
    // CR 503.1a: the upkeep trigger is on the stack before P1 gets priority,
    // controlled by P0 — so the recipient below is not the ability controller.
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == karona && entry.controller == P0),
        "Karona's P0-controlled upkeep trigger must be on the stack at P1's upkeep; stack={:?}",
        runner.state().stack
    );
    let before = &runner.state().objects[&karona];
    assert_eq!(
        before.controller, P0,
        "reach-guard: P0 controls Karona pre-resolution"
    );
    assert!(
        before.tapped,
        "reach-guard: Karona is still tapped pre-resolution"
    );

    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "Karona's trigger must resolve; stopped on {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().phase, Phase::Upkeep);
    let object = &runner.state().objects[&karona];
    assert_eq!(object.controller, P1, "P1 must gain control of Karona");
    assert!(
        !object.tapped,
        "Karona's trigger must untap it before the handoff"
    );
}
