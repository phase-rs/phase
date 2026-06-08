//! Regression (issue #1985): Modal DFC commanders must offer a cast-time face
//! choice when cast from the command zone (CR 712.11b + CR 903.8).

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

#[test]
fn issue_1985_peter_parker_commander_offers_modal_face_choice_from_command_zone() {
    let Some(db) = load_db() else { return };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario.add_real_card(P0, "Peter Parker", Zone::Hand, db);
    scenario.with_commander(card);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let back = runner
        .state()
        .objects
        .get(&card)
        .and_then(|o| o.back_face.clone())
        .expect("command-zone commander must hydrate modal back face");
    assert_eq!(back.name, "Amazing Spider-Man");
    assert_eq!(back.layout_kind, Some(LayoutKind::Modal));

    let cast_actions = engine::ai_support::legal_actions(runner.state())
        .iter()
        .filter(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == card))
        .count();
    assert_eq!(
        cast_actions, 1,
        "commander must be offered as castable from the command zone before casting"
    );

    let card_id = runner.state().objects[&card].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
        })
        .expect("CastSpell on commander from command zone accepted");
    assert!(
        matches!(result.waiting_for, WaitingFor::ModalFaceChoice { .. }),
        "commander modal DFC must enter ModalFaceChoice from command zone; got {:?}",
        result.waiting_for
    );

    runner
        .act(GameAction::ChooseModalFace { back_face: true })
        .expect("ChooseModalFace{back} from command zone accepted");
    runner.advance_until_stack_empty();

    assert!(
        runner
            .battlefield_names()
            .iter()
            .any(|n| n == "Amazing Spider-Man"),
        "back face must resolve from command-zone commander cast; battlefield = {:?}",
        runner.battlefield_names()
    );
}
