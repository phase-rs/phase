//! Runtime (engine-path) regression for Tomb of Annihilation "Veils of Fear".
//!
//! Oracle: "Each player loses 2 life unless they discard a card." The room was
//! wrongly implemented as "each player sacrifices a creature" — the wrong
//! keyword action entirely, never touching life. This drives the real venture
//! pipeline: with the marker in Trapped Entry (room 0 → branch to [1 Veils of
//! Fear, 2 Oubliette]), a venture + room choice enters Veils of Fear. Each
//! player is then offered the CR 118.12a punisher choice ("discard a card" or
//! lose 2 life); with empty hands both decline and lose 2 life. On the old code
//! the room sacrificed a creature and never touched life, so the assertions
//! below fail — this discriminates the real behavior.

use engine::game::dungeon::DungeonId;
use engine::game::effects::venture;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

#[test]
fn tomb_veils_of_fear_each_player_loses_two_life_at_runtime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    // Both hands are empty (GameScenario::new deals no opening hand), so neither
    // player can pay the "discard a card" alternative and each must take the
    // life loss when prompted.
    let mut runner = scenario.build();

    // Marker in Trapped Entry (Tomb room 0 → [1 Veils of Fear, 2 Oubliette]).
    {
        let prog = runner.state_mut().dungeon_progress.entry(P0).or_default();
        prog.current_dungeon = Some(DungeonId::TombOfAnnihilation);
        prog.current_room = 0;
    }

    // Venture → branch → choose Veils of Fear (room 1).
    let venture_ability =
        ResolvedAbility::new(Effect::VentureIntoDungeon, vec![], ObjectId(99), P0);
    let mut events = Vec::new();
    venture::resolve(runner.state_mut(), &venture_ability, &mut events).unwrap();
    runner
        .act(GameAction::ChooseDungeonRoom { room_index: 1 })
        .expect("choosing Veils of Fear must succeed");
    assert_eq!(
        runner.state().dungeon_progress[&P0].current_room,
        1,
        "venture must enter Veils of Fear (room 1)"
    );

    // Resolve the room ability. It puts a per-player "loses 2 life unless they
    // discard a card" punisher on the stack (CR 118.12a). Resolve it, then
    // decline each player's discard-or-lose-life prompt so the life loss lands.
    for _ in 0..12 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            runner
                .act(GameAction::PayUnlessCost { pay: false })
                .expect("declining the discard alternative must succeed");
        } else if !runner.state().stack.is_empty() {
            runner.resolve_top();
        } else {
            break;
        }
    }

    assert_eq!(
        runner.state().players[0].life,
        18,
        "the venturing player loses 2 life (declined the discard); the old room \
         sacrificed a creature and never touched life"
    );
    assert_eq!(
        runner.state().players[1].life,
        18,
        "each other player also loses 2 life (the room affects each player)"
    );
}
