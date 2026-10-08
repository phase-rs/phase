//! Runtime (engine-path) regression for GitHub issue #9143 — Dungeon of the
//! Mad Mage's final room, Mad Wizard's Lair, never resolved in live games.
//!
//! The room effect itself was implemented for #4251, but only exercised via
//! direct `resolve_ability_chain`. On the live path the room trigger goes
//! through trigger dispatch, whose announcement slot-building treated the
//! chained `CastFromZone { target: LastRevealed }` ("cast one of them") as a
//! player-chosen target. With nothing revealed yet it found zero legal
//! candidates and dropped the whole trigger (`DroppedNoLegalRequiredTarget`),
//! so venturing 7 → 8 drew nothing and the SBA completion masked the fizzle.
//!
//! This drives the actual venture pipeline: marker at Deep Mines (room 7),
//! venture into Mad Wizard's Lair (room 8), resolve the room trigger, and
//! verify the three draws plus the optional free-cast prompt.

use engine::game::dungeon::DungeonId;
use engine::game::effects::venture;
use engine::game::engine::apply_as_current;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

#[test]
fn mad_wizards_lair_trigger_reaches_stack_and_draws_three() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["A", "B", "C", "D", "E"]);
    let mut runner = scenario.build();

    // Marker already in Mad Mage at Deep Mines (room 7 → [8] Mad Wizard's Lair).
    {
        let prog = runner.state_mut().dungeon_progress.entry(P0).or_default();
        prog.current_dungeon = Some(DungeonId::DungeonOfTheMadMage);
        prog.current_room = 7;
    }

    // Venture: advance into Mad Wizard's Lair (room 8). Pre-fix the room
    // trigger was dropped here and the stack stayed empty.
    let venture_ability =
        ResolvedAbility::new(Effect::VentureIntoDungeon, vec![], ObjectId(99), P0);
    let mut events = Vec::new();
    venture::resolve(runner.state_mut(), &venture_ability, &mut events).unwrap();
    assert_eq!(
        runner.state().dungeon_progress[&P0].current_room,
        8,
        "venture must advance the marker into Mad Wizard's Lair (room 8)"
    );
    assert!(
        !runner.state().stack.is_empty(),
        "Lair room trigger must reach the stack (it was dropped pre-fix)"
    );

    // Resolve the room ability: draw three and reveal them, then offer the
    // optional free cast.
    let hand_before = runner.state().players[0].hand.len();
    runner.resolve_top();
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 3,
        "Lair must draw three cards"
    );
    assert_eq!(
        runner.state().last_revealed_ids.len(),
        3,
        "the three drawn cards must be recorded as revealed"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "optional free cast must pause for accept/decline, got {:?}",
        runner.state().waiting_for
    );

    // Declining keeps all three drawn cards and ends the chain cleanly.
    apply_as_current(
        runner.state_mut(),
        GameAction::DecideOptionalEffect { accept: false },
    )
    .expect("decline optional free cast");
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 3,
        "declining keeps all three drawn cards"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority, got {:?}",
        runner.state().waiting_for
    );
}
