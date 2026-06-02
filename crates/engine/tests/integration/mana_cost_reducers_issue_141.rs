//! Regression for issue #141: Delighted Halfling restricted mana + Goblin
//! Anarchomancer cost reduction.
//!
//! https://github.com/phase-rs/phase/issues/141

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::mana_payment::can_pay_for_spell;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::create_object;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{
    ManaCost, ManaRestriction, ManaType, ManaUnit, PaymentContext,
};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn load_db() -> Option<&'static CardDatabase> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
    if !path.exists() {
        let data_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/card-data.json");
        if !data_path.exists() {
            return None;
        }
        static DB_DATA: OnceLock<CardDatabase> = OnceLock::new();
        return Some(DB_DATA.get_or_init(|| {
            CardDatabase::from_export(&data_path).expect("data/card-data.json should load")
        }));
    }
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    Some(DB.get_or_init(|| CardDatabase::from_export(&path).expect("card-data.json should load")))
}

/// Doors of Durin is {3}{R}{G}; Goblin Anarchomancer should reduce generic by 1.
#[test]
fn goblin_anarchomancer_reduces_doors_of_durin_display_cost() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Goblin Anarchomancer", Zone::Battlefield, db);
    let doors = scenario.add_real_card(P0, "Doors of Durin", Zone::Hand, db);

    let mut game = scenario.build();
    engine::game::rehydrate_game_from_card_db(game.state_mut(), db);

    let cost =
        engine::game::casting::display_spell_cost(game.state(), P0, doors).expect("display cost");
    let ManaCost::Cost { generic, .. } = cost else {
        panic!("expected ManaCost::Cost, got {cost:?}");
    };
    assert_eq!(
        generic, 2,
        "Doors of Durin should display {2}{R}{G} with Anarchomancer on board (issue #141)"
    );
}

/// Delighted Halfling mana with legendary restriction must pay for a commander.
#[test]
fn delighted_halfling_restricted_mana_pays_legendary_commander() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let halfling = scenario.add_real_card(P0, "Delighted Halfling", Zone::Battlefield, db);
    let commander = scenario.add_real_card(P0, "Frodo, Adventurous Hobbit", Zone::Command, db);

    let mut game = scenario.build();
    engine::game::rehydrate_game_from_card_db(game.state_mut(), db);

    // Tap Halfling for restricted mana (simulate the second ability resolving).
    let restriction = ManaRestriction::OnlyForSpellType("Legendary".to_string());
    game.state_mut().players[P0].mana_pool.mana.push(ManaUnit::new(
        ManaType::Red,
        halfling,
        false,
        vec![restriction],
    ));

    let meta = engine::game::casting::build_spell_meta(game.state(), P0, commander)
        .expect("commander spell meta");
    assert!(
        meta.types
            .iter()
            .any(|t| t.eq_ignore_ascii_case("Legendary")),
        "commander meta must include Legendary supertype"
    );

    let cost = engine::game::casting::display_spell_cost(game.state(), P0, commander)
        .expect("commander display cost");
    let spell_ctx = PaymentContext::Spell(&meta);
    assert!(
        can_pay_for_spell(
            &game.state().players[P0].mana_pool,
            &cost,
            Some(&spell_ctx),
            engine::game::static_abilities::build_cost_permission_context(
                game.state(),
                P0,
                false,
            ),
        ),
        "legendary-restricted mana must be eligible for commander cast (issue #141)"
    );
}

/// Manual regression without card-data: Halfling-style restriction + commander types.
#[test]
fn legendary_restricted_mana_allows_commander_spell_meta() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut game = scenario.build();
    let state = game.state_mut();
    let commander_id = create_object(
        state,
        CardId(9001),
        P0,
        "Test Commander".to_string(),
        Zone::Command,
    );
    {
        let obj = state.objects.get_mut(&commander_id).unwrap();
        obj.card_types.supertypes.push(Supertype::Legendary);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.is_commander = true;
    }

    let restriction = ManaRestriction::OnlyForSpellType("Legendary".to_string());
    state.players[P0].mana_pool.mana.push(ManaUnit::new(
        ManaType::Green,
        ObjectId(1),
        false,
        vec![restriction],
    ));

    let meta = engine::game::casting::build_spell_meta(game.state(), P0, commander_id).unwrap();
    let spell_ctx = PaymentContext::Spell(&meta);
    let cost = ManaCost::generic(3);
    assert!(
        can_pay_for_spell(
            &game.state().players[P0].mana_pool,
            &cost,
            Some(&spell_ctx),
            engine::game::static_abilities::build_cost_permission_context(
                game.state(),
                P0,
                false,
            ),
        )
    );
}
