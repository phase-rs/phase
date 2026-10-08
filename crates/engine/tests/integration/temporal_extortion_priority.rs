//! Reproduction coverage for Temporal Extortion's cast trigger, optional
//! payment, extra turn, and viewer-priority projection.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const TEMPORAL_EXTORTION_ORACLE: &str = "When you cast this spell, any player may pay half their life, rounded up. If a player does, counter Temporal Extortion.\nTake an extra turn after this one.";

fn mana(n: usize, mana_type: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
        .collect()
}

fn pass_through_empty_turn(
    runner: &mut GameRunner,
    player: engine::types::PlayerId,
    turns_taken_before: u32,
) {
    for _ in 0..128 {
        if runner.state().active_player == player
            && runner.state().players[player.0 as usize].turns_taken > turns_taken_before
        {
            return;
        }
        if runner.state().active_player != player {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => runner
                .act(GameAction::PassPriority)
                .expect("priority pass during an empty turn"),
            WaitingFor::DeclareAttackers { .. } => runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("empty attackers declaration"),
            WaitingFor::DeclareBlockers { .. } => runner
                .act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .expect("empty blockers declaration"),
            other => panic!("unexpected waiting state while ending empty turn: {other:?}"),
        };
    }
    panic!("turn did not finish within the guard");
}

/// CR 117.3b + CR 500.7: after both players decline the cast trigger, the
/// spell resolves, grants its controller an extra turn, and both viewer
/// projections agree that controller has priority in that turn.
#[test]
fn temporal_extortion_declined_by_all_starts_extra_turn_with_shared_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let temporal_extortion = scenario
        .add_spell_to_hand_from_oracle(P0, "Temporal Extortion", false, TEMPORAL_EXTORTION_ORACLE)
        .id();
    // The original turn must reach its draw step before its queued extra turn
    // starts, so neither player may deck out during this focused scenario.
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest"]);
    scenario.with_mana_pool(P0, mana(3, ManaType::Colorless));
    scenario.with_mana_pool(P0, mana(1, ManaType::Black));

    let mut runner = scenario.build();
    let mut cast = runner.cast(temporal_extortion).commit();
    let mut offered_to = Vec::new();

    for _ in 0..24 {
        match cast.state().waiting_for.clone() {
            WaitingFor::OpponentMayChoice { player, .. } => {
                offered_to.push(player);
                cast.act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("declining Temporal Extortion's payment must succeed");
            }
            WaitingFor::Priority { .. } if offered_to.len() < 2 => {
                cast.act(GameAction::PassPriority)
                    .expect("priority pass before Temporal Extortion resolves");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected Temporal Extortion resolution state: {other:?}"),
        }
    }
    assert_eq!(
        offered_to,
        vec![P0, P1],
        "any player must be offered the payment in APNAP order",
    );

    let outcome = cast.resolve();
    assert!(
        outcome
            .state()
            .extra_turns
            .iter()
            .any(|turn| turn.player == P0),
        "all declines must allow Temporal Extortion to enqueue P0's extra turn",
    );

    let mut after_resolution = GameRunner::from_state(outcome.state().clone());
    let original_turns_taken = after_resolution.state().players[P0.0 as usize].turns_taken;
    pass_through_empty_turn(&mut after_resolution, P0, original_turns_taken);

    assert_eq!(after_resolution.state().active_player, P0);
    assert!(
        after_resolution.state().players[P0.0 as usize].turns_taken > original_turns_taken,
        "P0 must begin the extra turn after their original turn ends",
    );
    assert!(matches!(
        after_resolution.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert_eq!(after_resolution.state().priority_player, P0);

    for viewer in [P0, P1] {
        let view = filter_state_for_viewer(after_resolution.state(), viewer);
        assert!(matches!(
            view.waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        assert_eq!(view.priority_player, P0);
    }
}
