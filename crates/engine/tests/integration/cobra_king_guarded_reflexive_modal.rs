//! The Cobra King's guarded reflexive modal.
//!
//! Its parent upkeep trigger first creates Cobra Coil. Only then can the
//! `When you do, if you control five or more Snakes and/or Serpents` modal
//! trigger be created and present its choice.

use super::rules::{GameRunner, GameScenario, Phase, WaitingFor, P0, P1};

const COBRA_KING_ORACLE: &str = "At the beginning of each player's upkeep, create a 1/1 blue Serpent creature token named Cobra Coil. When you do, if you control five or more Snakes and/or Serpents, choose one —\n• Strike first — Target Snake or Serpent you control fights target creature an opponent controls.\n• Strike hard — Put a +1/+1 counter on each Snake and Serpent you control.";

/// Start P0's upkeep with the real Cobra King subtype plus `other_snakes`.
/// The parent creates one Serpent before its reflexive guard is evaluated.
fn cobra_king_upkeep(other_snakes: usize) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario
        .add_creature_from_oracle(P0, "The Cobra King", 5, 2, COBRA_KING_ORACLE)
        .with_subtypes(vec!["Snake", "Hawk", "Warrior"]);
    for index in 0..other_snakes {
        scenario
            .add_creature(P0, &format!("Snake {index}"), 1, 1)
            .with_subtypes(vec!["Snake"]);
    }
    // Keeps Strike first's fight mode available, so the positive case proves
    // the real two-mode modal can be presented rather than merely materialized.
    scenario.add_creature(P1, "Opponent Creature", 2, 2);

    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    runner
}

#[test]
fn cobra_king_creates_its_reflexive_modal_after_cobra_coil_reaches_five() {
    // Cobra King + three Snakes + Cobra Coil = five after the parent resolves.
    let runner = cobra_king_upkeep(3);

    let WaitingFor::AbilityModeChoice { player, modal, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "five Snakes and/or Serpents after Cobra Coil must present the reflexive modal, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "the Cobra King's controller chooses the mode");
    assert_eq!(modal.mode_count, 2, "both Cobra King modes must be present");
}

#[test]
fn cobra_king_does_not_create_a_reflexive_modal_below_five() {
    // Cobra King + two Snakes + Cobra Coil = four: the guard fails after the
    // parent resolves, so no separate reflexive trigger can present a modal.
    let runner = cobra_king_upkeep(2);

    assert!(
        runner
            .state()
            .battlefield
            .iter()
            .any(|id| runner.state().objects[id].name == "Cobra Coil"),
        "the parent trigger must create Cobra Coil before the reflexive guard fails"
    );

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. }
        ),
        "four Snakes and/or Serpents must not present Cobra King's modal"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the failed guard must leave no reflexive modal trigger on the stack"
    );
}
