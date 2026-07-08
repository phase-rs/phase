//! Runtime (engine-path) regression for Tomb of Annihilation "Sandfall Cell".
//!
//! Oracle: "Each player loses 2 life unless they sacrifice a creature, artifact,
//! or land of their choice." The room was wrongly implemented as "you lose 2
//! life and create a 2/2 black Zombie creature token" — a fabricated token that
//! appears nowhere on the card, and it only touched the controller. This drives
//! the real venture pipeline: with the marker in Veils of Fear (room 1 → [3]
//! Sandfall Cell), a venture enters Sandfall Cell. Each player is offered the CR
//! 118.12a punisher choice; with no permanents to sacrifice, both decline and
//! lose 2 life. On the old code only the controller lost life and a bogus token
//! was created, so the assertions below fail — this discriminates the real
//! behavior.

use engine::game::dungeon::DungeonId;
use engine::game::effects::venture;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::actions::{GameAction, UnlessCostBranch};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

#[test]
fn tomb_sandfall_cell_each_player_loses_two_life_at_runtime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    // Neither player controls any permanent, so neither can pay the "sacrifice a
    // creature, artifact, or land" alternative and each must take the life loss.
    let mut runner = scenario.build();

    // Marker in Veils of Fear (Tomb room 1 → [3] Sandfall Cell, a single exit).
    {
        let prog = runner.state_mut().dungeon_progress.entry(P0).or_default();
        prog.current_dungeon = Some(DungeonId::TombOfAnnihilation);
        prog.current_room = 1;
    }

    // Venture → single exit → Sandfall Cell (room 3).
    let venture_ability =
        ResolvedAbility::new(Effect::VentureIntoDungeon, vec![], ObjectId(99), P0);
    let mut events = Vec::new();
    venture::resolve(runner.state_mut(), &venture_ability, &mut events).unwrap();
    assert_eq!(
        runner.state().dungeon_progress[&P0].current_room,
        3,
        "venture must enter Sandfall Cell (room 3)"
    );

    // Resolve: each player is prompted (single UnlessPayment or a disjunctive
    // UnlessPaymentChooseCost). With no permanents, decline so the life loss
    // lands.
    for _ in 0..12 {
        match &runner.state().waiting_for {
            WaitingFor::UnlessPayment { .. } => {
                runner
                    .act(GameAction::PayUnlessCost { pay: false })
                    .expect("declining must succeed");
            }
            WaitingFor::UnlessPaymentChooseCost { .. } => {
                runner
                    .act(GameAction::ChooseUnlessCostBranch {
                        choice: UnlessCostBranch::Decline,
                    })
                    .expect("declining the disjunctive cost must succeed");
            }
            _ if !runner.state().stack.is_empty() => {
                runner.resolve_top();
            }
            _ => break,
        }
    }

    assert_eq!(
        runner.state().players[0].life,
        18,
        "the venturing player loses 2 life (declined the sacrifice); the old room \
         only touched the controller and created a bogus Zombie token"
    );
    assert_eq!(
        runner.state().players[1].life,
        18,
        "each other player also loses 2 life (the room affects each player)"
    );
}
