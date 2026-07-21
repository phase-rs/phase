//! Runtime (engine-path) regression for Lost Mine of Phandelver "Fungi Cavern".
//!
//! Oracle: "Target creature gets -4/-0 until your next turn." The room used
//! `Duration::UntilEndOfTurn`, ending the debuff a full turn early. This drives
//! the venture pipeline and the REAL turn machinery to prove the -4/-0:
//!   1. applies when the room resolves,
//!   2. SURVIVES the controller's end-of-turn cleanup (into the opponent's
//!      turn) — the discriminator vs the old until-end-of-turn behavior,
//!   3. EXPIRES at the beginning of the controller's next turn.

use engine::game::dungeon::DungeonId;
use engine::game::effects::venture;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

fn power_of(runner: &GameRunner, obj: ObjectId) -> Option<i32> {
    runner.state().objects[&obj].power
}

/// Drive the REAL turn machinery until `turn_number` reaches `target_turn`
/// (declaring no attackers/blockers, draining trigger ordering, no-op cleanup
/// discards). Mirrors the cross-turn harness in
/// `momir_token_firebreathing_duration.rs`.
fn advance_to_turn(runner: &mut GameRunner, target_turn: u32) {
    for _ in 0..400 {
        if runner.state().turn_number >= target_turn {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declaring no attackers must be accepted");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declaring no blockers must be accepted");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DiscardChoice { .. } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("no-op cleanup discard must be accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected WaitingFor while advancing turns: {other:?}"),
        }
    }
    panic!("failed to reach turn {target_turn}");
}

#[test]
fn fungi_cavern_debuff_survives_end_of_turn_and_expires_next_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Libraries so neither player decks out while advancing across turns.
    scenario.with_library_top(P0, &["L0a", "L0b", "L0c", "L0d", "L0e"]);
    scenario.with_library_top(
        engine::game::scenario::P1,
        &["L1a", "L1b", "L1c", "L1d", "L1e"],
    );
    // The only creature (so Fungi Cavern auto-targets it): a 4/4 → 0/4 under -4/-0.
    let creature = scenario.add_creature(P0, "Big Bear", 4, 4).id();
    let mut runner = scenario.build();
    let start_turn = runner.state().turn_number;

    // Marker in Mine Tunnels (Lost Mine room 2 → [4 Dark Pool, 5 Fungi Cavern]).
    {
        let prog = runner.state_mut().dungeon_progress.entry(P0).or_default();
        prog.current_dungeon = Some(DungeonId::LostMineOfPhandelver);
        prog.current_room = 2;
    }

    // Venture → choose Fungi Cavern (room 5); its -4/-0 ability auto-targets the
    // only creature and goes on the stack. Resolve it.
    let venture_ability =
        ResolvedAbility::new(Effect::VentureIntoDungeon, vec![], ObjectId(99), P0);
    let mut events = Vec::new();
    venture::resolve(runner.state_mut(), &venture_ability, &mut events).unwrap();
    runner
        .act(GameAction::ChooseDungeonRoom { room_index: 5 })
        .expect("choosing Fungi Cavern must succeed");
    assert_eq!(runner.state().dungeon_progress[&P0].current_room, 5);
    runner.resolve_top();

    // -4/-0 applied: power 4 → 0.
    assert_eq!(
        power_of(&runner, creature),
        Some(0),
        "-4/-0 applies (4 - 4 = 0)"
    );

    // Survives the controller's own end-of-turn cleanup, into the opponent's
    // turn. The old `Duration::UntilEndOfTurn` would restore the power to 4 here.
    advance_to_turn(&mut runner, start_turn + 1);
    assert_eq!(
        power_of(&runner, creature),
        Some(0),
        "the -4/-0 persists through end of turn ('until your next turn')"
    );

    // Expires at the beginning of the controller's next turn.
    advance_to_turn(&mut runner, start_turn + 2);
    assert_eq!(
        power_of(&runner, creature),
        Some(4),
        "the -4/-0 expires at the controller's next turn"
    );
}
