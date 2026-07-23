//! Runtime (engine-path) regression for Lost Mine of Phandelver "Storeroom".
//!
//! Oracle: "Put a +1/+1 counter on target creature" — ANY creature. The room
//! was wrongly restricted to "creature you control", so when the venturing
//! player controlled no creatures the ability had no legal target (removed on
//! resolution, CR 608.2b). This drives the venture pipeline with the controller
//! controlling NO creatures and the opponent controlling one, and proves the
//! opponent's creature is targeted and receives the counter — behavior the old
//! restriction made impossible.

use engine::game::dungeon::DungeonId;
use engine::game::effects::venture;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

#[test]
fn lost_mine_storeroom_counters_opponent_creature_at_runtime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0 (venturing) controls NO creatures; the opponent P1 controls one.
    let opp = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let mut runner = scenario.build();

    // Marker in Goblin Lair (Lost Mine room 1 → [3 Storeroom, 4 Dark Pool]).
    {
        let prog = runner.state_mut().dungeon_progress.entry(P0).or_default();
        prog.current_dungeon = Some(DungeonId::LostMineOfPhandelver);
        prog.current_room = 1;
    }

    // Venture → branch → choose Storeroom (room 3).
    let venture_ability =
        ResolvedAbility::new(Effect::VentureIntoDungeon, vec![], ObjectId(99), P0);
    let mut events = Vec::new();
    venture::resolve(runner.state_mut(), &venture_ability, &mut events).unwrap();
    runner
        .act(GameAction::ChooseDungeonRoom { room_index: 3 })
        .expect("choosing Storeroom must succeed");
    assert_eq!(
        runner.state().dungeon_progress[&P0].current_room,
        3,
        "venture must enter Storeroom (room 3)"
    );

    // "+1/+1 counter on target creature" auto-targets the only legal creature —
    // the opponent's (the controller has none) — and is on the stack. Under the
    // old "creature you control" restriction there is no legal target, so the
    // ability is removed on resolution (CR 608.2b) and no counter is placed.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Storeroom's targeted ability must be on the stack with the opponent's creature chosen"
    );
    runner.resolve_top();

    assert_eq!(
        runner.state().objects[&opp]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "the opponent's creature must receive Storeroom's +1/+1 counter"
    );
}
