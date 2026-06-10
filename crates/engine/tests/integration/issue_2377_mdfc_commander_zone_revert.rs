//! Regression (issue #2377): An MDFC commander cast as its back face must revert
//! to its front face when it returns to the command zone (CR 712.8a).

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// CR 712.8a + CR 903.9a: A commander MDFC cast as its back face must show
/// front-face characteristics when it reaches the command zone — the command
/// zone is "a zone other than the battlefield or stack."
#[test]
fn issue_2377_mdfc_commander_reverts_to_front_face_on_return_to_command_zone() {
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

    // Cast Peter Parker and choose the back face (Amazing Spider-Man).
    let card_id = runner.state().objects[&card].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell on Peter Parker accepted");
    assert!(
        matches!(result.waiting_for, WaitingFor::ModalFaceChoice { .. }),
        "casting a modal DFC must enter ModalFaceChoice; got {:?}",
        result.waiting_for
    );
    runner
        .act(GameAction::ChooseModalFace { back_face: true })
        .expect("ChooseModalFace{back} accepted");
    runner.advance_until_stack_empty();

    // The spell resolves; the object keeps its ID through zone transitions.
    // Verify Amazing Spider-Man is on the battlefield showing modal_back_face.
    assert!(
        runner
            .battlefield_names()
            .iter()
            .any(|n| n == "Amazing Spider-Man"),
        "Amazing Spider-Man must be on the battlefield after resolving back face; got {:?}",
        runner.battlefield_names()
    );
    let battlefield_id = runner
        .state()
        .objects
        .values()
        .find(|o| o.zone == Zone::Battlefield && o.name == "Amazing Spider-Man")
        .map(|o| o.id)
        .expect("Amazing Spider-Man must be findable by zone+name in objects map");
    assert!(
        runner.state().objects[&battlefield_id].modal_back_face,
        "modal_back_face must be set while Amazing Spider-Man is on the battlefield"
    );

    // Simulate the commander dying (battlefield → graveyard).
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        battlefield_id,
        Zone::Graveyard,
        &mut events,
    );

    // CR 712.8a: the graveyard revert fires during move_to_zone; verify before SBA.
    let obj = &runner.state().objects[&battlefield_id];
    assert_eq!(
        obj.zone,
        Zone::Graveyard,
        "commander must be in the graveyard"
    );
    assert!(
        !obj.modal_back_face,
        "modal_back_face must be cleared on exit from the battlefield (CR 712.8a)"
    );
    assert_eq!(
        obj.name, "Peter Parker",
        "must show front face in the graveyard (CR 712.8a)"
    );

    // CR 903.9a: SBA raises the commander zone return choice.
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CommanderZoneChoice { .. }
        ),
        "SBA must offer the command zone return choice; got {:?}",
        runner.state().waiting_for
    );

    // Accept: the commander moves to the command zone.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("DecideOptionalEffect{accept} accepted");

    // CR 712.8a: in the command zone, must show front face.
    let obj = &runner.state().objects[&battlefield_id];
    assert_eq!(
        obj.zone,
        Zone::Command,
        "commander must be in the command zone"
    );
    assert!(
        !obj.modal_back_face,
        "modal_back_face must remain cleared in the command zone (CR 712.8a)"
    );
    assert_eq!(
        obj.name, "Peter Parker",
        "must show front face (Peter Parker) in the command zone"
    );
}
