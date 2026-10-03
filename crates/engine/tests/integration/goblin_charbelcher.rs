//! CR 701.20a + CR 608.2c + CR 120.3: Goblin Charbelcher — "{3}, {T}: Reveal
//! cards from the top of your library until you reveal a land card. This
//! artifact deals damage equal to the number of nonland cards revealed this way
//! to any target. If the revealed land card was a Mountain, this artifact deals
//! double that damage instead. Put the revealed cards on the bottom of your
//! library in any order."
//!
//! The revealed land card is NOT put into a hand: every revealed card, the land
//! included, goes to the library bottom.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CHARBELCHER: &str = "{3}, {T}: Reveal cards from the top of your library until you reveal a land card. Goblin Charbelcher deals damage equal to the number of nonland cards revealed this way to any target. If the revealed land card was a Mountain, Goblin Charbelcher deals double that damage instead. Put the revealed cards on the bottom of your library in any order.";

/// A library staged top-first as `nonland_count` nonland cards, then a land with
/// `land_subtype`, then one untouched card. Returns the revealed cards in
/// encounter order plus the untouched card.
fn stage(
    nonland_count: usize,
    land_name: &str,
    land_subtype: &str,
) -> (GameRunner, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Goblin Charbelcher", CHARBELCHER);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );

    // `library[0]` is the top: stage bottom-up.
    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let land = scenario
        .add_spell_to_library_top(P0, land_name, false)
        .as_land()
        .with_subtypes(vec![land_subtype])
        .id();
    let mut revealed = vec![land];
    for i in 0..nonland_count {
        let spell = scenario
            .add_spell_to_library_top(P0, &format!("Spell {i}"), false)
            .id();
        revealed.insert(0, spell);
    }
    (scenario.build(), revealed, deep)
}

fn charbelcher(runner: &GameRunner) -> ObjectId {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Goblin Charbelcher")
        .expect("Goblin Charbelcher on the battlefield")
}

/// Answer the library-bottom ordering prompt (if the engine raises one) with
/// the given order, then let the stack empty.
fn finish(runner: &mut GameRunner, order: &[ObjectId]) {
    if let WaitingFor::EffectZoneChoice { cards, .. } = &runner.state().waiting_for {
        assert_eq!(
            cards.len(),
            order.len(),
            "the whole remaining pile is ordered"
        );
        runner
            .act(GameAction::SelectCards {
                cards: order.to_vec(),
            })
            .expect("bottom order is accepted");
    }
    runner.advance_until_stack_empty();
}

/// Activate targeting P1 and answer the library-bottom ordering prompt.
fn activate_and_finish(runner: &mut GameRunner, revealed: &[ObjectId]) {
    let source = charbelcher(runner);
    runner.activate(source, 0).target_player(P1).resolve();
    finish(runner, revealed);
}

fn assert_all_on_bottom_and_none_in_hand(
    runner: &GameRunner,
    revealed: &[ObjectId],
    deep: ObjectId,
) {
    let library: Vec<ObjectId> = runner.state().players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    let mut expected = vec![deep];
    expected.extend_from_slice(revealed);
    assert_eq!(
        library, expected,
        "the revealed cards, land included, go to the bottom of the library"
    );
    for id in revealed {
        assert_eq!(runner.state().objects[id].zone, Zone::Library);
    }
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "no revealed card is put into a hand"
    );
}

#[test]
fn deals_damage_equal_to_nonland_cards_revealed() {
    let (mut runner, revealed, deep) = stage(3, "Forest", "Forest");
    let life_before = runner.state().players[P1.0 as usize].life;

    activate_and_finish(&mut runner, &revealed);

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before - 3,
        "three nonland cards were revealed before the Forest"
    );
    assert_all_on_bottom_and_none_in_hand(&runner, &revealed, deep);
}

#[test]
fn mountain_doubles_the_damage() {
    let (mut runner, revealed, deep) = stage(3, "Mountain", "Mountain");
    let life_before = runner.state().players[P1.0 as usize].life;

    activate_and_finish(&mut runner, &revealed);

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before - 6,
        "a revealed Mountain doubles the three nonland cards' damage"
    );
    assert_all_on_bottom_and_none_in_hand(&runner, &revealed, deep);
}

#[test]
fn land_on_top_reveals_only_the_land_and_deals_no_damage() {
    let (mut runner, revealed, deep) = stage(0, "Mountain", "Mountain");
    let life_before = runner.state().players[P1.0 as usize].life;

    activate_and_finish(&mut runner, &revealed);

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before,
        "zero nonland cards revealed: zero damage, doubled or not"
    );
    assert_all_on_bottom_and_none_in_hand(&runner, &revealed, deep);
}

const SWANS: &str = "If a source would deal damage to this creature, prevent that damage. The source's controller draws cards equal to the damage prevented this way.";

/// CR 608.2c + CR 615.5: the revealed pile is put on the bottom AFTER the damage
/// instruction and its replacement have been carried out. Swans of Bryn Argoll
/// prevents the damage and draws that many cards at once; with the pile still in
/// place the draw takes the three revealed nonland cards from the top. Placing
/// the pile first would bury them and draw the deep card instead.
#[test]
fn pile_is_placed_after_damage_and_its_replacement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Goblin Charbelcher", CHARBELCHER);
    let swans = scenario
        .add_creature_from_oracle(P1, "Swans of Bryn Argoll", 4, 3, SWANS)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let forest = scenario
        .add_spell_to_library_top(P0, "Forest", false)
        .as_land()
        .with_subtypes(vec!["Forest"])
        .id();
    let mut drawn = Vec::new();
    for i in 0..3 {
        let spell = scenario
            .add_spell_to_library_top(P0, &format!("Spell {i}"), false)
            .id();
        drawn.insert(0, spell);
    }
    let mut runner = scenario.build();

    let source = charbelcher(&runner);
    runner.activate(source, 0).target_object(swans).resolve();

    // Damage and the Swans replacement have already run: the three nonland cards
    // are in hand, and only the land remains to be placed. Nothing was ordered
    // onto the bottom before the damage.
    let mut hand: Vec<ObjectId> = runner.state().players[P0.0 as usize]
        .hand
        .iter()
        .copied()
        .collect();
    hand.sort();
    let mut expected = drawn.clone();
    expected.sort();
    assert_eq!(
        hand, expected,
        "the Swans draw takes the revealed nonland cards"
    );
    assert_eq!(
        runner.state().objects[&swans].damage_marked,
        0,
        "the damage was prevented by Swans's replacement"
    );

    finish(&mut runner, &[forest]);
    let library: Vec<ObjectId> = runner.state().players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        library,
        vec![deep, forest],
        "the revealed land is bottomed last"
    );
}

/// CR 401.2 + CR 701.20a: the revealed set is the engine's own "revealed this
/// way" population, in reveal order. Its members end in hidden library
/// positions, so no audience's serialized state may carry their ids — while the
/// engine's own copy keeps the full population for its quantity readers.
#[test]
fn revealed_population_is_not_exposed_in_any_audiences_payload() {
    use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};

    let (mut runner, revealed, _deep) = stage(3, "Forest", "Forest");
    activate_and_finish(&mut runner, &revealed);

    let engine_set_members: Vec<ObjectId> = runner
        .state()
        .tracked_object_sets
        .values()
        .flatten()
        .copied()
        .collect();
    for id in &revealed {
        assert!(
            engine_set_members.contains(id),
            "reach guard: the engine keeps the full revealed population"
        );
    }

    for (label, projected) in [
        ("P0", filter_state_for_viewer(runner.state(), P0)),
        ("P1", filter_state_for_viewer(runner.state(), P1)),
        ("unseated", filter_state_for_unseated_viewer(runner.state())),
    ] {
        let payload = serde_json::to_value(&projected).expect("projected state serializes");
        let carried: Vec<u64> = payload["tracked_object_sets"]
            .as_object()
            .expect("tracked sets serialize as a map")
            .values()
            .flat_map(|members| members.as_array().expect("members").iter())
            .filter_map(serde_json::Value::as_u64)
            .collect();
        for id in &revealed {
            assert!(
                !carried.contains(&id.0),
                "{label}: tracked_object_sets carries the library card {id:?}"
            );
        }
    }
}
