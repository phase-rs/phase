//! Regression for issue #4358 — Hama Pashar must double dungeon room abilities.
//!
//! https://github.com/phase-rs/phase/issues/4358

use engine::game::dungeon::DungeonId;
use engine::game::effects::venture::handle_choose_room;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::phase::Phase;
use engine::types::statics::{StaticMode, TriggerCause};

const HAMA_PASHAR: &str = "Room abilities of dungeons you own trigger an additional time.";

fn hand_len(runner: &engine::game::scenario::GameRunner) -> usize {
    runner.state().players[P0.0 as usize].hand.len()
}

#[test]
fn hama_pashar_parses_room_ability_trigger_doubler() {
    let parsed = parse_oracle_text(
        HAMA_PASHAR,
        "Hama Pashar, Ruin Seeker",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Wizard".to_string()],
    );
    let static_def = parsed
        .statics
        .first()
        .expect("Hama Pashar oracle must parse as a static");
    assert!(matches!(
        static_def.mode,
        StaticMode::DoubleTriggers {
            cause: TriggerCause::RoomEntered
        }
    ));
}

#[test]
fn hama_pashar_doubles_lost_mine_draw_room() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Hama Pashar, Ruin Seeker", 2, 3, HAMA_PASHAR);
    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Card {i}"));
    }

    let mut runner = scenario.build();
    {
        let progress = runner.state_mut().dungeon_progress.entry(P0).or_default();
        progress.current_dungeon = Some(DungeonId::LostMineOfPhandelver);
        progress.current_room = 6;
    }

    let hand_before = hand_len(&runner);
    let mut events = Vec::new();
    handle_choose_room(
        runner.state_mut(),
        P0,
        DungeonId::LostMineOfPhandelver,
        6,
        &mut events,
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_len(&runner),
        hand_before + 2,
        "Temple of Dumathoin draw room must resolve twice with Hama Pashar"
    );
}
