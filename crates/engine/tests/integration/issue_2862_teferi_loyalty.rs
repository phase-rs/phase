//! Regression for issue #2862: Teferi, Time Raveler must lose loyalty from
//! combat damage and pay loyalty costs after entering through the
//! real cast / battlefield-entry pipelines with intrinsic loyalty counters.
//!
//! https://github.com/phase-rs/phase/issues/2862

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::AbilityCost;
use engine::types::counter::CounterType;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn issue_2862_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_2862_cards.json");
        engine::database::card_db::CardDatabase::from_export(&path)
            .expect("issue_2862_cards.json fixture must load")
    })
}

fn add_mana(runner: &mut engine::game::scenario::GameRunner, player: PlayerId, mana: &[ManaType]) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

#[test]
fn issue_2862_teferi_cast_minus_three_pays_loyalty_cost() {
    let db = issue_2862_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let teferi_spell = scenario.add_real_card(P0, "Teferi, Time Raveler", Zone::Hand, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(
        &mut runner,
        P0,
        &[
            ManaType::White,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );

    runner.cast(teferi_spell).resolve();

    let teferi = teferi_spell;
    assert_eq!(runner.state().objects[&teferi].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&teferi]
            .counters
            .get(&CounterType::Loyalty)
            .copied(),
        Some(4),
        "cast Teferi must seed intrinsic loyalty counters"
    );

    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert_eq!(
        runner.state().objects[&teferi]
            .counters
            .get(&CounterType::Loyalty)
            .copied(),
        Some(4),
        "rehydrate must preserve or repair loyalty counters"
    );

    let mut events = Vec::new();
    engine::game::casting::pay_ability_cost(
        runner.state_mut(),
        P0,
        teferi,
        &AbilityCost::Loyalty { amount: -3 },
        &mut events,
    )
    .expect("pay [-3] loyalty cost through activation payment seam");

    assert_eq!(runner.state().objects[&teferi].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&teferi].loyalty, Some(1));
    assert_eq!(
        runner.state().objects[&teferi]
            .counters
            .get(&CounterType::Loyalty)
            .copied(),
        Some(1)
    );
}

#[test]
fn issue_2862_teferi_combat_damage_removes_loyalty() {
    let db = issue_2862_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let teferi = scenario.add_real_card(P0, "Teferi, Time Raveler", Zone::Battlefield, db);
    let attacker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert_eq!(
        runner.state().objects[&teferi]
            .counters
            .get(&CounterType::Loyalty)
            .copied(),
        Some(4),
        "battlefield Teferi must carry intrinsic loyalty counters"
    );

    runner.advance_to_combat();

    runner
        .declare_attackers(&[(attacker, AttackTarget::Planeswalker(teferi))])
        .expect("declare attacker at Teferi");

    for _ in 0..16 {
        if runner.state().objects[&teferi].loyalty == Some(2) {
            break;
        }
        runner.pass_both_players();
    }

    assert_eq!(
        runner.state().objects[&teferi].loyalty,
        Some(2),
        "2 combat damage must remove 2 loyalty (4 → 2)"
    );
    assert_eq!(
        runner.state().objects[&teferi]
            .counters
            .get(&CounterType::Loyalty)
            .copied(),
        Some(2)
    );
}
