//! Regression: casting an Adventure card whose creature face has no spell
//! ability (only static abilities) panicked with "adventure spell must have
//! ability_def" because `handle_adventure_choice` re-implemented the cast
//! pipeline and unwrapped the spell ability for both faces.
//!
//! Elusive Otter (creature: Otter with Prowess + a CantBlock static; adventure:
//! Grove's Bounty, sorcery) is the canonical reproducer.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::CastingVariant;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_db() -> Option<&'static CardDatabase> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        return None;
    }
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    Some(DB.get_or_init(|| CardDatabase::from_export(&path).expect("export should load")))
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

#[test]
fn elusive_otter_creature_face_cast_does_not_panic() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let otter_id = scenario.add_real_card(P0, "Elusive Otter", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana(&mut runner, &[ManaType::Blue, ManaType::Green]);

    let card_id = runner.state().objects[&otter_id].card_id;
    let r1 = runner
        .act(GameAction::CastSpell {
            object_id: otter_id,
            card_id,
            targets: vec![],
        })
        .expect("cast should be accepted");
    assert!(
        matches!(r1.waiting_for, WaitingFor::AdventureCastChoice { .. }),
        "Expected AdventureCastChoice, got {:?}",
        r1.waiting_for
    );

    runner
        .act(GameAction::ChooseAdventureFace { creature: true })
        .expect("creature face cast should succeed without panic");

    // CR 601.2a: spell is on the stack with the creature face's casting variant.
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == otter_id)
        .expect("otter should be on the stack after creature-face cast");
    match entry.kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => {
            assert_eq!(
                casting_variant,
                CastingVariant::Normal,
                "creature-face cast must use Normal variant, not Adventure"
            );
        }
        ref other => panic!("expected Spell entry, got {other:?}"),
    }
}

#[test]
fn elusive_otter_adventure_face_cast_does_not_panic() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let otter_id = scenario.add_real_card(P0, "Elusive Otter", Zone::Hand, db);
    // Grove's Bounty needs a legal creature target.
    let _bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana(&mut runner, &[ManaType::Green]);

    let card_id = runner.state().objects[&otter_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: otter_id,
            card_id,
            targets: vec![],
        })
        .expect("cast should be accepted");
    runner
        .act(GameAction::ChooseAdventureFace { creature: false })
        .expect("adventure face cast should succeed");
    // X-cost commit (X=0; finalize_cast stamps the final variant onto the
    // stack entry only after the X choice).
    runner
        .act(GameAction::ChooseX { value: 0 })
        .expect("X=0 commit should succeed");

    // CR 715.3a: finalized on the stack with the Adventure variant so it
    // resolves to exile (not graveyard) and remembers the alternative-cast
    // permission for later creature-face casts from exile.
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == otter_id)
        .expect("otter should be on the stack after adventure-face cast");
    match entry.kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => {
            assert_eq!(casting_variant, CastingVariant::Adventure);
        }
        ref other => panic!("expected Spell entry, got {other:?}"),
    }
}
