//! Runtime regression test for Curse of the Restless Dead trigger filtering.
//!
//! Curse of the Restless Dead is an Aura — Curse that enchants a player and
//! triggers "Whenever a land enchanted player controls enters, you create a
//! 2/2 black Zombie creature token with decayed."
//!
//! This test verifies:
//! 1. The trigger fires when a land enters under the enchanted player's control
//!    and creates a 2/2 black Zombie token with decayed under the curse
//!    controller's control (positive case).
//! 2. The trigger does NOT fire when a land enters under a different player's
//!    control (negative case).
//!
//! CR references:
//!   - CR 303.4b: An Aura that enchants a player is attached to that player.
//!   - CR 603.6a: Zone-change triggers use the game state after the event to
//!     determine if they should trigger.
//!   - CR 702.147a: Decayed means "This creature can't block. When it attacks,
//!     sacrifice it at end of combat."
use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::game::zones::move_to_zone;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Oracle text for Curse of the Restless Dead (Innistrad: Midnight Hunt).
const CURSE_ORACLE: &str =
    "Whenever a land enchanted player controls enters, you create a 2/2 black Zombie creature token with decayed.";

/// Count triggered abilities on the stack sourced from `source`.
fn stack_triggers_from(runner: &engine::game::scenario::GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == source)
        .count()
}

/// Count 2/2 black Zombie tokens with decayed on the battlefield controlled by `player`.
fn zombie_token_count(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|obj| {
                obj.zone == Zone::Battlefield
                    && obj.controller == player
                    && obj.is_token
                    && obj.power == Some(2)
                    && obj.toughness == Some(2)
                    && obj.card_types.subtypes.iter().any(|s| s == "Zombie")
                    && obj.color.contains(&ManaColor::Black)
                    && obj.has_keyword(&Keyword::Decayed)
            })
        })
        .count()
}

/// Set up a scenario with Curse of the Restless Dead attached to P1,
/// controlled by P0. Returns `(runner, curse_id)`.
fn setup_curse_on_p1() -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Curse of the Restless Dead", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE_ORACLE);
        builder.id()
    };
    // Add library padding so advance_until_stack_empty doesn't deck anyone.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }
    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);
    (runner, curse_id)
}

/// Create a land in `owner`'s hand and move it to the battlefield.
fn enter_land(
    runner: &mut engine::game::scenario::GameRunner,
    owner: engine::types::player::PlayerId,
    name: &str,
) {
    let land_id = {
        let state = runner.state_mut();
        let card_id = engine::types::identifiers::CardId(state.next_object_id);
        let id =
            engine::game::zones::create_object(state, card_id, owner, name.to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
        id
    };
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), land_id, Zone::Battlefield, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());
}

#[test]
fn curse_of_the_restless_dead_fires_when_enchanted_player_land_enters() {
    let (mut runner, curse_id) = setup_curse_on_p1();
    assert_eq!(zombie_token_count(&runner, P0), 0);

    // A land enters under P1 (the enchanted player).
    enter_land(&mut runner, P1, "Forest");

    // The curse trigger must be on the stack.
    assert_eq!(
        stack_triggers_from(&runner, curse_id),
        1,
        "Curse of the Restless Dead must trigger when enchanted player's land enters"
    );

    // Resolve the trigger — a 2/2 black Zombie token with decayed should be
    // created under P0 (the curse controller).
    runner.advance_until_stack_empty();
    assert_eq!(
        zombie_token_count(&runner, P0),
        1,
        "curse controller (P0) must have a 2/2 Zombie token after trigger resolves"
    );
}

#[test]
fn curse_of_the_restless_dead_does_not_fire_for_non_enchanted_player() {
    let (mut runner, curse_id) = setup_curse_on_p1();
    assert_eq!(zombie_token_count(&runner, P0), 0);

    // A land enters under P0 (the NON-enchanted player / curse controller).
    enter_land(&mut runner, P0, "Mountain");

    // The curse must NOT trigger for P0's land.
    assert_eq!(
        stack_triggers_from(&runner, curse_id),
        0,
        "Curse of the Restless Dead must NOT trigger when non-enchanted player's land enters"
    );

    // No tokens created.
    assert_eq!(
        zombie_token_count(&runner, P0),
        0,
        "no Zombie token should exist"
    );
}
