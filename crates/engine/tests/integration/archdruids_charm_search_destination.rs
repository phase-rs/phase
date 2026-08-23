//! Archdruid's Charm — mode one searches either a creature or land, then uses
//! the found card's type to choose its destination.
//!
//! Oracle: "Choose one —
//! • Search your library for a creature or land card and reveal it. Put it onto
//! the battlefield tapped if it's a land card. Otherwise, put it into your hand.
//! Then shuffle.
//! • Put a +1/+1 counter on target creature you control. It deals damage equal
//! to its power to target creature you don't control.
//! • Exile target artifact or enchantment."

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ARCHDRUIDS_CHARM_ORACLE: &str = "Choose one —\n\
• Search your library for a creature or land card and reveal it. Put it onto the battlefield tapped if it's a land card. Otherwise, put it into your hand. Then shuffle.\n\
• Put a +1/+1 counter on target creature you control. It deals damage equal to its power to target creature you don't control.\n\
• Exile target artifact or enchantment.";

fn green_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![
            ManaCostShard::Green,
            ManaCostShard::Green,
            ManaCostShard::Green,
        ],
        generic: 0,
    }
}

fn green_mana_pool() -> Vec<ManaUnit> {
    (0..3)
        .map(|index| ManaUnit::new(ManaType::Green, ObjectId(9_000 + index), false, vec![]))
        .collect()
}

fn add_library_card(runner: &mut GameRunner, name: &str, card_type: CoreType) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        name.to_string(),
        Zone::Library,
    );
    let object = runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("new library card must exist");
    object.card_types.core_types.push(card_type);
    object.base_card_types = object.card_types.clone();
    id
}

fn setup() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Archdruid's Charm", true, ARCHDRUIDS_CHARM_ORACLE)
        .with_mana_cost(green_cost())
        .id();
    scenario.with_mana_pool(P0, green_mana_pool());

    let mut runner = scenario.build();
    let creature = add_library_card(&mut runner, "Llanowar Elves", CoreType::Creature);
    let land = add_library_card(&mut runner, "Forest", CoreType::Land);
    (runner, charm, creature, land)
}

fn resolve_mode_one_to_search(
    runner: &mut GameRunner,
    charm: ObjectId,
    creature: ObjectId,
    land: ObjectId,
) {
    let outcome = runner.cast(charm).modes(&[0]).resolve();
    let WaitingFor::SearchChoice { cards, .. } = outcome.final_waiting_for() else {
        panic!(
            "Archdruid's Charm mode one must reach SearchChoice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert!(
        cards.contains(&creature) && cards.contains(&land),
        "the creature-or-land search must offer both library cards, got {cards:?}"
    );
    assert!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .mana
            .is_empty(),
        "casting Archdruid's Charm must spend its exact {{G}}{{G}}{{G}} cost"
    );
}

#[test]
fn mode_one_puts_the_selected_land_onto_the_battlefield_tapped() {
    let (mut runner, charm, creature, land) = setup();
    resolve_mode_one_to_search(&mut runner, charm, creature, land);

    // CR 701.23a + CR 608.2c: select the search result, then resolve the
    // conditional continuation against that selected card.
    runner
        .act(GameAction::SelectCards { cards: vec![land] })
        .expect("select the land from Archdruid's Charm's search");

    let state = runner.state();
    assert_eq!(state.objects[&land].zone, Zone::Battlefield);
    assert!(
        state.objects[&land].tapped,
        "the selected land enters tapped"
    );
    assert_eq!(state.objects[&creature].zone, Zone::Library);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
}

#[test]
fn mode_one_puts_the_selected_creature_into_hand_not_the_battlefield() {
    let (mut runner, charm, creature, land) = setup();
    resolve_mode_one_to_search(&mut runner, charm, creature, land);

    // CR 701.23a + CR 608.2c: select the search result, then resolve the
    // conditional continuation against that selected card.
    runner
        .act(GameAction::SelectCards {
            cards: vec![creature],
        })
        .expect("select the creature from Archdruid's Charm's search");

    let state = runner.state();
    assert_eq!(state.objects[&creature].zone, Zone::Hand);
    assert_ne!(state.objects[&creature].zone, Zone::Battlefield);
    assert_eq!(state.objects[&land].zone, Zone::Library);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
}
