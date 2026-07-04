//! Runtime (engine-path) regression for Dungeon of the Mad Mage "Lost Level".
//!
//! The room was stubbed as `Effect::Unimplemented`, so venturing into it did
//! nothing. Its real Oracle effect is "Scry 2". This drives the actual venture
//! pipeline: with the marker in Goblin Bazaar (room 2 → [4] Lost Level), a
//! venture advances into Lost Level, its room ability goes on the stack, and
//! resolving it produces a Scry over the top TWO library cards
//! (`WaitingFor::ScryChoice`). On the old stub no scry happens and the match
//! below fails — so this discriminates the real game behavior, not just the AST.

use engine::game::dungeon::DungeonId;
use engine::game::effects::venture;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

#[test]
fn mad_mage_lost_level_scries_two_at_runtime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A library to scry (needs at least 2 cards).
    scenario.with_library_top(P0, &["Top A", "Top B", "Top C"]);
    let mut runner = scenario.build();

    // Marker already in Mad Mage at Goblin Bazaar (room 2 → [4] Lost Level).
    {
        let prog = runner.state_mut().dungeon_progress.entry(P0).or_default();
        prog.current_dungeon = Some(DungeonId::DungeonOfTheMadMage);
        prog.current_room = 2;
    }

    // Venture: advance into Lost Level (room 4); its Scry 2 ability goes on the stack.
    let venture_ability =
        ResolvedAbility::new(Effect::VentureIntoDungeon, vec![], ObjectId(99), P0);
    let mut events = Vec::new();
    venture::resolve(runner.state_mut(), &venture_ability, &mut events).unwrap();
    assert_eq!(
        runner.state().dungeon_progress[&P0].current_room,
        4,
        "venture must advance the marker into Lost Level (room 4)"
    );

    // Resolve the room ability — a real Scry 2 asks the controller to arrange the
    // top TWO cards of their library.
    runner.resolve_top();
    match &runner.state().waiting_for {
        WaitingFor::ScryChoice { player, cards } => {
            assert_eq!(*player, P0, "the venturing player scries");
            assert_eq!(
                cards.len(),
                2,
                "Lost Level scries 2 (the old Unimplemented stub scried nothing)"
            );
        }
        other => panic!("expected a Scry-2 ScryChoice after resolving Lost Level, got {other:?}"),
    }
}
