//! Regression (issue #9505): the single "[subject] is put into exile from
//! <zone>" trigger arm must fail closed on an origin it cannot parse.
//!
//! Before the fix, "from an opponent's library" was silently dropped, leaving
//! an unconstrained `ChangesZone` → Exile trigger that fired on exile from ANY
//! zone. These tests drive the real pipeline: the watcher is built from Oracle
//! text, and a real Cremate resolves through the stack to exile a card from
//! its owner's graveyard. The watcher's "you lose 1 life" is the witness — only
//! its trigger drains its controller's life.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Exiles a creature card from P0's graveyard with Cremate while a watcher
/// carrying `watcher_oracle` is on P0's battlefield, and returns how much life
/// P0 lost.
fn life_lost_after_graveyard_exile(watcher_oracle: &str) -> i32 {
    let db = load_db().expect("shared card database must be available for this integration test");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Exile Watcher", 1, 1)
        .from_oracle_text(watcher_oracle);
    let cremate = scenario.add_real_card(P0, "Cremate", Zone::Hand, db);
    let graveyard_card = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    // Draw fodder so Cremate's "draw a card" does not deck P0 out.
    scenario.add_real_card(P0, "Forest", Zone::Library, db);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let life_before = runner.state().players[0].life;
    let outcome = runner.cast(cremate).target_object(graveyard_card).resolve();
    let state = outcome.state();

    assert_eq!(
        state.objects[&graveyard_card].zone,
        Zone::Exile,
        "Cremate must exile the targeted card from the graveyard"
    );
    life_before - state.players[0].life
}

/// Positive control: a recognized origin fires on a matching exile, proving
/// the scenario reaches the trigger matcher.
#[test]
fn recognized_exile_origin_fires_on_matching_exile() {
    let life_lost = life_lost_after_graveyard_exile(
        "Whenever a creature card is put into exile from your graveyard, you lose 1 life.",
    );
    assert_eq!(
        life_lost, 1,
        "graveyard exile must fire the from-your-graveyard watcher"
    );
}

/// The unparseable origin must not degrade into a fire-from-anywhere trigger:
/// exiling from a graveyard leaves the watcher silent.
#[test]
fn unparseable_exile_origin_does_not_fire_from_another_zone() {
    let life_lost = life_lost_after_graveyard_exile(
        "Whenever a creature card is put into exile from an opponent's library, you lose 1 life.",
    );
    assert_eq!(
        life_lost, 0,
        "an unparseable exile origin must not fire on exile from a graveyard"
    );
}
