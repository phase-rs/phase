//! Regression for issue #2863: Auras must not persist on a commander that left
//! the battlefield and returned via the command zone; commander must not duplicate
//! in graveyard when dying.
//!
//! https://github.com/phase-rs/phase/issues/2863

use engine::game::deck_loading::create_object_from_card_face;
use engine::game::effects::attach::attach_to;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::{add_to_zone, remove_from_zone};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn issue_2863_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_2863_cards.json");
        engine::database::card_db::CardDatabase::from_export(&path)
            .expect("issue_2863_cards.json fixture must load")
    })
}

/// Place a real card on the battlefield after setup, bypassing unattached-aura SBA.
fn place_on_battlefield(
    state: &mut engine::types::game_state::GameState,
    player: engine::types::player::PlayerId,
    name: &str,
    db: &engine::database::card_db::CardDatabase,
) -> ObjectId {
    let face = db
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("card '{}' not found in fixture", name));
    let id = create_object_from_card_face(state, face, player);
    remove_from_zone(state, id, Zone::Library, player);
    add_to_zone(state, id, Zone::Battlefield, player);
    state.objects.get_mut(&id).unwrap().zone = Zone::Battlefield;
    id
}

#[test]
fn issue_2863_aura_put_in_graveyard_when_commander_exiled_and_returns() {
    let db = issue_2863_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arcades = scenario.add_real_card(P0, "Arcades, the Strategist", Zone::Hand, db);
    scenario.with_commander(arcades);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let card_id = runner.state().objects[&arcades].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: arcades,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast Arcades");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&arcades].zone, Zone::Battlefield);

    let inviolability = place_on_battlefield(runner.state_mut(), P0, "Inviolability", db);
    attach_to(runner.state_mut(), inviolability, arcades);
    assert_eq!(
        runner.state().objects[&inviolability].attached_to,
        Some(arcades.into())
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), arcades, Zone::Exile, &mut events);

    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    while matches!(
        runner.state().waiting_for,
        WaitingFor::CommanderZoneChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept commander zone return");
        engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    }

    assert_eq!(
        runner.state().objects[&inviolability].zone,
        Zone::Graveyard,
        "Inviolability must be in graveyard after commander left battlefield"
    );
    assert!(
        runner.state().objects[&inviolability].attached_to.is_none(),
        "aura must not remain attached after CR 704.5n"
    );
    assert_eq!(
        runner.state().objects[&arcades].zone,
        Zone::Command,
        "commander must return to command zone from exile"
    );

    runner.state_mut().players[0].mana_pool.mana = vec![
        ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    ];

    let card_id = runner.state().objects[&arcades].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: arcades,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("recast Arcades");
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&arcades].zone, Zone::Battlefield);
    assert!(
        !runner.state().objects[&arcades]
            .attachments
            .contains(&inviolability),
        "recast commander must not list the old aura as attached"
    );
}

#[test]
fn issue_2863_commander_not_duplicated_in_graveyard_on_death() {
    let db = issue_2863_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let arcades = scenario.add_real_card(P0, "Arcades, the Strategist", Zone::Hand, db);
    scenario.with_commander(arcades);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.state_mut().format_config.command_zone = true;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let card_id = runner.state().objects[&arcades].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: arcades,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast Arcades");
    runner.advance_until_stack_empty();

    let inviolability = place_on_battlefield(runner.state_mut(), P0, "Inviolability", db);
    attach_to(runner.state_mut(), inviolability, arcades);

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), arcades, Zone::Graveyard, &mut events);
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CommanderZoneChoice { .. }
        ),
        "must offer commander zone return"
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("return commander to command zone");

    let graveyard_arcades = runner.state().players[0]
        .graveyard
        .iter()
        .filter(|id| runner.state().objects[id].name == "Arcades, the Strategist")
        .count();
    assert_eq!(
        graveyard_arcades, 0,
        "commander must not remain in graveyard after returning to command zone"
    );
    assert_eq!(runner.state().objects[&arcades].zone, Zone::Command);

    assert_eq!(
        runner.state().objects[&inviolability].zone,
        Zone::Graveyard,
        "aura must be in graveyard after host died"
    );
    assert!(
        runner.state().objects[&inviolability].attached_to.is_none(),
        "aura must not retain a dangling attachment pointer in the graveyard"
    );
}
