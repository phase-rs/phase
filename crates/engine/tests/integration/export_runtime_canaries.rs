use std::path::Path;
use std::sync::OnceLock;

use assert_matches::assert_matches;
use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_export_fixture() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/runtime_card_export_fixture.json");
        CardDatabase::from_export(&path).expect("export fixture should load")
    })
}

#[test]
fn export_fixture_loads_runtime_faces() {
    let db = load_export_fixture();
    assert!(db.get_face_by_name("Forest").is_some());
    assert!(db.get_face_by_name("Lightning Bolt").is_some());
    assert!(db.get_face_by_name("Grizzly Bears").is_some());
}

#[test]
fn export_backed_lightning_bolt_canary() {
    let db = load_export_fixture();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forest_id = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let bolt_id = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let mut runner = scenario.build();

    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .expect("player should exist")
        .mana_pool
        .add(ManaUnit::new(ManaType::Red, forest_id, false, vec![]));

    let card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: bolt_id,
            card_id,
            targets: vec![],
        })
        .expect("export-backed bolt cast should succeed");
    assert_matches!(result.waiting_for, WaitingFor::TargetSelection { .. });

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1)],
        })
        .expect("selecting bolt target should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P1), 17);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn export_backed_grizzly_bears_combat_canary() {
    let db = load_export_fixture();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears_id = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();

    runner.pass_both_players();
    assert_matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    );

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(bears_id, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attackers should succeed");

    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("declare no blockers should succeed");
    }
    for _ in 0..20 {
        if runner.life(P1) < 20 {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }

    assert_eq!(runner.life(P1), 18);
}
