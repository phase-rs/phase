//! Runtime regression test for `ControllerRef::EnchantedPlayer` trigger filtering.
//!
//! Trespasser's Curse is an Aura — Curse that enchants a player and triggers
//! "Whenever a creature enchanted player controls enters, that player loses 1
//! life and you gain 1 life."
//!
//! This test verifies:
//! 1. The trigger fires when a creature enters under the enchanted player's
//!    control (positive case).
//! 2. The trigger does NOT fire when a creature enters under a different
//!    player's control (negative case).
//!
//! CR references:
//!   - CR 303.4b: An Aura that enchants a player is attached to that player.
//!   - CR 603.6a: Zone-change triggers use the game state after the event to
//!     determine if they should trigger.
//!   - CR 702.5a: "Enchant [quality]" defines what an Aura can be attached to.

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::game::zones::move_to_zone;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Oracle text for Trespasser's Curse (Amonkhet).
const TRESPASSERS_CURSE_ORACLE: &str =
    "Whenever a creature enchanted player controls enters, that player loses 1 life and you gain 1 life.";

/// Count triggered abilities on the stack sourced from `source`.
fn stack_triggers_from(runner: &engine::game::scenario::GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == source)
        .count()
}

/// Set up a scenario with Trespasser's Curse attached to P1, controlled by P0.
/// Returns `(runner, curse_id)`.
fn setup_curse_on_p1() -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Create the curse as an enchantment on the battlefield under P0's control,
    // then attach it to P1.
    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Trespasser's Curse", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(TRESPASSERS_CURSE_ORACLE);
        builder.id()
    };

    // Add library padding so advance_until_stack_empty doesn't deck anyone.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();

    // Attach the curse to P1 (the enchanted player).
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());

    // Re-index triggers so the trigger system can find the curse's trigger
    // definitions after attachment changed the source context.
    reindex_object_triggers(runner.state_mut(), curse_id);

    (runner, curse_id)
}

#[test]
fn trespassers_curse_fires_when_enchanted_player_creature_enters() {
    let (mut runner, curse_id) = setup_curse_on_p1();

    let life_p0_before = runner.life(P0);
    let life_p1_before = runner.life(P1);

    // Create a creature in P1's hand, then move it to the battlefield.
    let creature = {
        let scenario_state = runner.state_mut();
        let card_id = engine::types::identifiers::CardId(scenario_state.next_object_id);
        let id = engine::game::zones::create_object(
            scenario_state,
            card_id,
            P1,
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        let obj = scenario_state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        id
    };

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), creature, Zone::Battlefield, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());

    // The curse trigger must be on the stack.
    assert_eq!(
        stack_triggers_from(&runner, curse_id),
        1,
        "Trespasser's Curse must trigger exactly once when enchanted player's creature enters"
    );

    // Resolve the trigger and verify life changes.
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        life_p1_before - 1,
        "enchanted player (P1) must lose 1 life"
    );
    assert_eq!(
        runner.life(P0),
        life_p0_before + 1,
        "curse controller (P0) must gain 1 life"
    );
}

#[test]
fn trespassers_curse_does_not_fire_for_non_enchanted_player() {
    let (mut runner, curse_id) = setup_curse_on_p1();

    let life_p0_before = runner.life(P0);
    let life_p1_before = runner.life(P1);

    // Create a creature in P0's hand (the NON-enchanted player), then move it
    // to the battlefield.
    let creature = {
        let scenario_state = runner.state_mut();
        let card_id = engine::types::identifiers::CardId(scenario_state.next_object_id);
        let id = engine::game::zones::create_object(
            scenario_state,
            card_id,
            P0,
            "Elvish Mystic".to_string(),
            Zone::Hand,
        );
        let obj = scenario_state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
        id
    };

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), creature, Zone::Battlefield, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());

    // The curse must NOT trigger for P0's creature.
    assert_eq!(
        stack_triggers_from(&runner, curse_id),
        0,
        "Trespasser's Curse must NOT trigger when non-enchanted player's creature enters"
    );

    // Life totals unchanged.
    assert_eq!(runner.life(P0), life_p0_before, "P0 life must be unchanged");
    assert_eq!(runner.life(P1), life_p1_before, "P1 life must be unchanged");
}
