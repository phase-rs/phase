//! Runtime pipeline regression for issue #720 — Saruman, the White Hand.
//!
//! Oracle text: "Whenever you cast a noncreature spell, amass Orcs X, where X
//! is that spell's mana value." Before the fix, the amass count-position
//! parser captured the bare "X" and discarded the trailing "where X is that
//! spell's mana value" clause, leaving X as an unresolved `Variable` ref. That
//! ref only resolves through a paid-X cost (which a noncreature spell like
//! Divination never has), so it silently evaluated to 0 — the Army token was
//! created but never received its +1/+1 counters, matching the Discord report
//! that the ability "wasn't amassing orcs."

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::counter::CounterType;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_mana(runner: &mut GameRunner, player: PlayerId, mana: &[ManaType]) {
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

fn army_plus1_counters(runner: &GameRunner, player: PlayerId) -> Option<u32> {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .find(|obj| obj.controller == player && obj.card_types.subtypes.iter().any(|s| s == "Army"))
        .map(|obj| {
            obj.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        })
}

/// CR 701.47a: Casting a noncreature spell amasses Orcs equal to that spell's
/// mana value — Divination ({2}{U}, mana value 3) must add 3 +1/+1 counters.
#[test]
fn saruman_white_hand_amasses_spell_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Saruman, the White Hand", Zone::Battlefield, db);
    let divination = scenario.add_real_card(P0, "Divination", Zone::Hand, db);
    for _ in 0..12 {
        scenario.add_card_to_library_top(P0, "Grizzly Bears");
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    add_mana(
        &mut runner,
        P0,
        &[ManaType::Blue, ManaType::Colorless, ManaType::Colorless],
    );

    assert_eq!(
        army_plus1_counters(&runner, P0),
        None,
        "no Army exists before any noncreature spell is cast"
    );

    runner.cast(divination).resolve();

    assert_eq!(
        army_plus1_counters(&runner, P0),
        Some(3),
        "casting Divination (mana value 3) must amass Orcs 3, not 0"
    );
}
