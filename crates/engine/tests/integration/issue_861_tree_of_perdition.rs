//! Regression for issue #861: Tree of Perdition's activated ability must
//! exchange target opponent's life with its toughness.
//!
//! https://github.com/phase-rs/phase/issues/861

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TREE_OF_PERDITION_ORACLE: &str =
    "Defender\n{T}: Exchange target opponent's life total with this creature's toughness.";

#[test]
fn tree_of_perdition_exchanges_opponent_life_with_its_toughness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tree = scenario
        .add_creature_from_oracle(P0, "Tree of Perdition", 0, 13, TREE_OF_PERDITION_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().players[1].life = 25;

    runner
        .act(GameAction::ActivateAbility {
            source_id: tree,
            ability_index: 0,
        })
        .expect("begin Tree of Perdition activation");

    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose target opponent");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected activation prompt: {other:?}"),
        }
    }

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[1].life,
        13,
        "opponent life should become Tree of Perdition's toughness"
    );
    assert_eq!(
        runner.state().objects[&tree].zone,
        Zone::Battlefield,
        "Tree of Perdition should remain on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&tree].toughness,
        Some(25),
        "Tree of Perdition's toughness should become the opponent's former life total"
    );
}
