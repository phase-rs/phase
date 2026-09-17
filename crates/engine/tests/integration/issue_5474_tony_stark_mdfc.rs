//! Regression (issue #5474): "Tony Stark // The Invincible Iron Man" is a
//! modal DFC whose two faces carry independently payable costs — {1}{U} front,
//! {4}{U}{R} back. With mana for BOTH faces on the table, the engine must offer
//! BOTH `ChooseModalFace` branches at the cast-time face choice, and must accept
//! the back-face branch so The Invincible Iron Man reaches the battlefield.
//!
//! The overlay that renders this prompt (`client/src/components/modal/ModalFaceModal.tsx`)
//! now paints exactly the faces the engine legalized, so these tests are the
//! engine-side lock on the data that overlay consumes: a regression that dropped
//! either branch from `legal_actions` would silently hide that face in the UI.
//!
//! Test A casts from hand; test B casts the same card as a commander from the
//! command zone (CR 903.8), where the printed cost is what's owed at tax 0.

use engine::ai_support::{legal_actions, legal_actions_full, stuck_decision_diagnostic};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// {C}{C}{C}{C}{U}{R} — enough for the front face ({1}{U}) *and* enough for the
/// back face ({4}{U}{R}), so neither branch can be excluded for affordability.
fn both_faces_payable_pool() -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
    ]
}

/// Reach guard: the object under test really is the modal DFC whose back face is
/// The Invincible Iron Man, hydrated as a Modal face (CR 712.3). Without this the
/// assertions below could pass on a card that never reaches the face choice.
fn assert_iron_man_back_face(runner: &GameRunner, card: ObjectId) {
    let back = runner
        .state()
        .objects
        .get(&card)
        .and_then(|o| o.back_face.clone())
        .expect("Tony Stark's modal back face must be hydrated");
    assert_eq!(back.name, "The Invincible Iron Man");
    assert_eq!(
        back.layout_kind,
        Some(LayoutKind::Modal),
        "back face must be Modal so the cast-face choice is offered"
    );
}

/// CR 712.11b: the caster chooses which face they are casting before putting the
/// spell onto the stack. CR 712.11c: only that face is evaluated to determine if
/// it can be cast — so with both faces payable, both branches must be offered
/// and the back-face branch must be accepted.
fn assert_both_faces_offered_and_back_face_resolves(runner: &mut GameRunner, card: ObjectId) {
    assert_iron_man_back_face(runner, card);

    let card_id = runner.state().objects[&card].card_id;
    assert!(
        legal_actions(runner.state()).iter().any(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == card)
        ),
        "Tony Stark must be offered as castable before the face choice; actions = {:?}",
        legal_actions(runner.state())
    );

    let result = runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell on Tony Stark accepted");
    assert!(
        matches!(result.waiting_for, WaitingFor::ModalFaceChoice { .. }),
        "casting a modal DFC must enter ModalFaceChoice; got {:?}",
        result.waiting_for
    );

    // CR 712.11c: each face is evaluated on its own, so a pool that pays either
    // one legalizes BOTH branches. This is the exact data the face-choice overlay
    // renders — dropping a branch here hides that face in the UI.
    let actions = legal_actions(runner.state());
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, GameAction::ChooseModalFace { back_face: false })),
        "front face ({{1}}{{U}}) must be legal with {{C}}{{C}}{{C}}{{C}}{{U}}{{R}}; actions = {actions:?}"
    );
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, GameAction::ChooseModalFace { back_face: true })),
        "back face ({{4}}{{U}}{{R}}) must be legal with {{C}}{{C}}{{C}}{{C}}{{U}}{{R}}; actions = {actions:?}"
    );

    // The prompt must be answerable by its authorized submitter: an empty
    // action set here would wedge the game (and paint an empty overlay).
    assert!(
        stuck_decision_diagnostic(runner.state()).is_none(),
        "ModalFaceChoice must not be a wedged decision point; diagnostic = {:?}",
        stuck_decision_diagnostic(runner.state())
    );
    let (full_actions, _, _) = legal_actions_full(runner.state());
    assert!(
        !full_actions.is_empty(),
        "ModalFaceChoice must publish a non-empty legal-action set"
    );

    runner
        .act(GameAction::ChooseModalFace { back_face: true })
        .expect("ChooseModalFace{back} accepted");
    runner.advance_until_stack_empty();

    assert!(
        runner
            .battlefield_names()
            .iter()
            .any(|n| n == "The Invincible Iron Man"),
        "the back face must resolve onto the battlefield; battlefield = {:?}",
        runner.battlefield_names()
    );
}

#[test]
fn issue_5474_tony_stark_offers_both_faces_from_hand() {
    let Some(db) = load_db() else { return };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tony = scenario.add_real_card(P0, "Tony Stark", Zone::Hand, db);
    scenario.with_mana_pool(P0, both_faces_payable_pool());

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert_eq!(
        runner.state().objects[&tony].zone,
        Zone::Hand,
        "test A must cast from hand"
    );
    assert_both_faces_offered_and_back_face_resolves(&mut runner, tony);
}

#[test]
fn issue_5474_tony_stark_offers_both_faces_from_command_zone() {
    let Some(db) = load_db() else { return };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tony = scenario.add_real_card(P0, "Tony Stark", Zone::Hand, db);
    scenario.with_commander(tony);
    scenario.with_mana_pool(P0, both_faces_payable_pool());

    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert_eq!(
        runner.state().objects[&tony].zone,
        Zone::Command,
        "test B must cast from the command zone"
    );
    // CR 903.8: a commander cast from the command zone costs an additional {2}
    // per previous command-zone cast. This is the first, so the tax is 0 and the
    // pool above is exactly what each face's printed cost demands.
    assert_eq!(
        engine::game::commander::commander_tax(runner.state(), tony),
        0,
        "first command-zone cast must be untaxed"
    );
    assert_both_faces_offered_and_back_face_resolves(&mut runner, tony);
}
