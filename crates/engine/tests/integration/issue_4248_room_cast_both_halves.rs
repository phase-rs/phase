//! Regression for GitHub issue #4248 — Spiked Corridor // Torture Pit: the
//! engine only let you play the front half (Spiked Corridor), never the second
//! door (Torture Pit).
//!
//! CR 709.3 / CR 709.5c: A Room is a split card whose two halves are each a
//! separately castable enchantment spell — "you may cast either half." Casting
//! a Room therefore needs the same cast-time face choice as a spell//spell
//! split (Life // Death); without it only the front (left-door) half is ever
//! reachable.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// {3}{R} for either half (both doors share the printed cost).
fn room_mana(owner: engine::types::identifiers::ObjectId) -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Red, owner, false, vec![]),
        ManaUnit::new(ManaType::Colorless, owner, false, vec![]),
        ManaUnit::new(ManaType::Colorless, owner, false, vec![]),
        ManaUnit::new(ManaType::Colorless, owner, false, vec![]),
    ]
}

#[test]
fn spiked_corridor_hydrates_torture_pit_back_half() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let room = scenario.add_real_card(P0, "Spiked Corridor", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let obj = runner.state().objects.get(&room).unwrap();
    assert_eq!(obj.name, "Spiked Corridor");
    assert_eq!(
        obj.back_face.as_ref().map(|b| b.name.as_str()),
        Some("Torture Pit"),
        "Spiked Corridor // Torture Pit must hydrate the second Room half"
    );
}

#[test]
fn spiked_corridor_casts_torture_pit_half_via_face_choice() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let room = scenario.add_real_card(P0, "Spiked Corridor", Zone::Hand, db);
    scenario.with_mana_pool(P0, room_mana(room));

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let commit = runner.cast(room).modal_back_face(true).commit();

    let stack_obj = commit
        .state()
        .stack
        .last()
        .map(|e| &commit.state().objects[&e.source_id]);
    let Some(spell) = stack_obj else {
        panic!(
            "the Torture Pit half must reach the stack — issue #4248 only allowed Spiked Corridor"
        );
    };
    assert_eq!(
        spell.name, "Torture Pit",
        "casting the second door must put Torture Pit on the stack, not Spiked Corridor"
    );
}

#[test]
fn spiked_corridor_torture_pit_half_resolves_to_battlefield_room() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let room = scenario.add_real_card(P0, "Spiked Corridor", Zone::Hand, db);
    scenario.with_mana_pool(P0, room_mana(room));

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let outcome = runner.cast(room).modal_back_face(true).resolve();

    let obj = outcome.state().objects.get(&room).unwrap();
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "the cast Room half must resolve onto the battlefield as a permanent"
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Room"),
        "the resolved permanent must be a Room"
    );
    // CR 709.5d: a Room enters with the unlocked designation for the half that
    // was cast. Torture Pit is the right half, so the right door is unlocked and
    // the left (Spiked Corridor) door stays locked.
    let unlocks = obj
        .room_unlocks
        .expect("a Room permanent tracks door state");
    assert!(
        unlocks.right_unlocked,
        "casting the right half (Torture Pit) must unlock the right door"
    );
    assert!(
        !unlocks.left_unlocked,
        "the uncast left half (Spiked Corridor) door must stay locked"
    );
}
