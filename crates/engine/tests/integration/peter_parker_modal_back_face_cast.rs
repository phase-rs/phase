//! "Peter Parker // Amazing Spider-Man" (SPM #10) — both "backside" cast paths,
//! driven through the real `apply` pipeline with cards loaded from the export.
//!
//! 1. The card is a modal DFC (that also transforms), so the BACK face
//!    "Amazing Spider-Man" must be castable directly from hand by choosing it at
//!    cast time (CR 712.3 + CR 712.11b).
//! 2. Once Amazing Spider-Man is in play, its static "Each legendary spell you
//!    cast that's one or more colors has web-slinging {G}{W}{U}" must actually
//!    OFFER and execute the granted web-slinging cast for a legendary
//!    multicolored creature in hand — paying {G}{W}{U} and returning a tapped
//!    creature you control (CR 702.188a + CR 604.1).

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
use engine::types::game_state::CastPaymentMode;

/// CR 712.11b: casting the modal DFC lets the caster choose the back face, so
/// Amazing Spider-Man (4/4) comes down directly from hand for {1}{G}{W}{U}.
#[test]
fn peter_parker_back_face_amazing_spider_man_is_castable_from_hand() {
    let Some(db) = load_db() else { return };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario.add_real_card(P0, "Peter Parker", Zone::Hand, db);
    scenario.with_life(P0, 20);
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
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // The modal back face must be hydrated as a Modal face for the cast-face
    // choice to appear (CR 712.3).
    let back = runner
        .state()
        .objects
        .get(&card)
        .and_then(|o| o.back_face.clone())
        .expect("Peter Parker's modal back face must be hydrated in hand");
    assert_eq!(back.name, "Amazing Spider-Man");
    assert_eq!(
        back.layout_kind,
        Some(LayoutKind::Modal),
        "back face must be Modal so the cast-face choice is offered"
    );

    // Casting the modal DFC enters the face choice; choose the back face.
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

    assert!(
        runner
            .battlefield_names()
            .iter()
            .any(|n| n == "Amazing Spider-Man"),
        "the back face Amazing Spider-Man must resolve onto the battlefield; battlefield = {:?}",
        runner.battlefield_names()
    );
}

/// CR 702.188a + CR 604.1: Amazing Spider-Man in play grants web-slinging
/// {G}{W}{U} to a legendary multicolored creature in hand; the option must be
/// offered and the creature must enter for {G}{W}{U} by returning a tapped
/// creature you control.
#[test]
fn amazing_spider_man_grants_web_slinging_to_a_big_legendary_creature_in_hand() {
    let Some(db) = load_db() else { return };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    // The back face (with its granting static) on the battlefield.
    let spidey = scenario.add_real_card(P0, "Amazing Spider-Man", Zone::Battlefield, db);
    // A big legendary multicolored creature to web-sling in (printed {U}{U}{U}{R}{R}{R}).
    let niv = scenario.add_real_card(P0, "Niv-Mizzet, Parun", Zone::Hand, db);
    // A creature tapped for mana, available to return as the web-sling cost.
    let mana_dork = scenario.add_vanilla(P0, 1, 1);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
        .state_mut()
        .objects
        .get_mut(&mana_dork)
        .unwrap()
        .tapped = true;

    assert!(
        !runner.state().objects[&spidey]
            .static_definitions
            .is_empty(),
        "Amazing Spider-Man must carry its web-slinging-granting static"
    );

    // The granted web-sling cast option must be offered for the hand creature.
    let actions = engine::ai_support::legal_actions(runner.state());
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::CastSpellAsWebSlinging { hand_object, .. } if *hand_object == niv
        )),
        "Amazing Spider-Man must grant a web-sling cast option to the legendary multicolored \
         creature in hand; actions referencing it = {:?}",
        actions
            .iter()
            .filter(|a| format!("{a:?}").contains(&format!("{niv:?}")))
            .collect::<Vec<_>>()
    );

    // Cast via web-slinging: pay {G}{W}{U} and return the tapped creature.
    let niv_card_id = runner.state().objects[&niv].card_id;
    runner
        .act(GameAction::CastSpellAsWebSlinging {
            hand_object: niv,
            card_id: niv_card_id,
            creature_to_return: mana_dork,

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("web-sling cast accepted");
    runner.advance_until_stack_empty();

    assert!(
        runner
            .battlefield_names()
            .iter()
            .any(|n| n == "Niv-Mizzet, Parun"),
        "the web-slung creature must resolve onto the battlefield; battlefield = {:?}",
        runner.battlefield_names()
    );
    assert_eq!(
        runner.state().objects.get(&mana_dork).map(|o| o.zone),
        Some(Zone::Hand),
        "the returned tapped creature must be back in its owner's hand"
    );
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .mana_pool
            .mana
            .len(),
        0,
        "the {{G}}{{W}}{{U}} web-sling cost must have been paid in full (not the printed cost)"
    );
}
