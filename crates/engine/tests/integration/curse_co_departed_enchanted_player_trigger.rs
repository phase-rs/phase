//! Regression test: Curse Aura co-departing with the enchanted player's creature
//! must still trigger via last-known-information (CR 603.10a).
//!
//! Scenario: P0 controls a Curse (modeled after Curse of Clinging Webs) attached
//! to P1. P1 controls a creature. A board wipe destroys both the Curse and the
//! creature simultaneously. The Curse's "Whenever a nontoken creature enchanted
//! player controls dies" trigger must still fire because the Curse was on the
//! battlefield when the creature died (CR 603.10a: an ability that triggers when
//! an object leaves the battlefield looks back to the game state immediately
//! before the event).
//!
//! The bug: `sever_battlefield_attachment_graph_on_exit` clears the Aura's
//! `attached_to` before `process_triggers` evaluates the co-departed observer.
//! `controller_ref_player` for `ControllerRef::EnchantedPlayer` reads the live
//! `attached_to` field, which is now `None`, so the trigger silently fails to
//! match.
//!
//! CR references:
//!   - CR 303.4b: An Aura that enchants a player is attached to that player.
//!   - CR 603.10a: An ability that triggers when an object leaves the battlefield
//!     uses the game state immediately before the event to determine if it should
//!     trigger. This applies to the observer (Curse) as well as the subject.
//!   - CR 704.5g + CR 704.7: Simultaneous SBA destruction.

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::types::card_type::CoreType;
use engine::types::game_state::StackEntryKind;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Oracle text for a Curse of Clinging Webs-style dies trigger.
const CURSE_DIES_ORACLE: &str =
    "Whenever a nontoken creature enchanted player controls dies, exile it.";

/// Count triggered abilities on the stack sourced from `source`.
fn stack_triggers_from(runner: &engine::game::scenario::GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == source
            )
        })
        .count()
}

/// Count triggers in the pending ordering queue sourced from `source`.
fn pending_triggers_from(runner: &engine::game::scenario::GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .pending_trigger_order
        .as_ref()
        .map(|pto| {
            pto.groups
                .iter()
                .flat_map(|g| g.triggers.iter())
                .filter(|t| t.pending.source_id == source)
                .count()
        })
        .unwrap_or(0)
}

/// CR 603.10a regression: A Curse that co-departs with the enchanted player's
/// creature in the same simultaneous event must still trigger.
#[test]
fn curse_co_departed_with_enchanted_players_creature_still_triggers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Create the curse as an enchantment on the battlefield under P0's control.
    // Use a creature shell and convert to enchantment (standard test pattern).
    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Curse of Clinging Webs", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE_DIES_ORACLE);
        builder.id()
    };

    // P1's creature that will die simultaneously with the Curse.
    let creature_id = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    // Library padding.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();

    // Attach the curse to P1.
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    // Verify setup: curse is on battlefield, attached to P1.
    assert_eq!(
        runner.state().objects[&curse_id].zone,
        Zone::Battlefield,
        "curse must start on battlefield"
    );
    assert!(
        runner.state().objects[&curse_id].attached_to.is_some(),
        "curse must be attached to P1"
    );

    // Simulate a board wipe: mark lethal damage on the creature. The Curse is an
    // enchantment (no toughness), so we must also give it a toughness and mark
    // lethal damage, OR we can make it a creature-enchantment. The simplest
    // approach: give the Curse object CoreType::Creature + toughness so SBA kills
    // it alongside the creature in one batch.
    {
        let curse_obj = runner.state_mut().objects.get_mut(&curse_id).unwrap();
        // Temporarily make it a creature so SBA lethal damage applies.
        if !curse_obj
            .card_types
            .core_types
            .contains(&CoreType::Creature)
        {
            curse_obj.card_types.core_types.push(CoreType::Creature);
        }
        curse_obj.toughness = Some(1);
        curse_obj.base_toughness = Some(1);
        curse_obj.damage_marked = 1;
    }
    {
        let creature_obj = runner.state_mut().objects.get_mut(&creature_id).unwrap();
        creature_obj.damage_marked = 2;
    }

    // Run SBAs — both the Curse and the creature die simultaneously.
    let mut sba_events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut sba_events);

    // Verify both moved to graveyard.
    assert_eq!(
        runner.state().objects[&curse_id].zone,
        Zone::Graveyard,
        "curse must be in graveyard after SBA"
    );
    assert_eq!(
        runner.state().objects[&creature_id].zone,
        Zone::Graveyard,
        "creature must be in graveyard after SBA"
    );

    // Verify attached_to was cleared (this is the bug condition).
    assert!(
        runner.state().objects[&curse_id].attached_to.is_none(),
        "attached_to is cleared after zone exit (this is the pre-existing behavior)"
    );

    // Process triggers from the SBA events. The Curse is a co-departed observer
    // of the creature's death. CR 603.10a says it should still trigger.
    process_triggers(runner.state_mut(), &sba_events);
    drain_order_triggers_with_identity(runner.state_mut());

    let on_stack = stack_triggers_from(&runner, curse_id);
    let in_pending = pending_triggers_from(&runner, curse_id);
    let total = on_stack + in_pending;

    assert_eq!(
        total, 1,
        "CR 603.10a: Curse that co-departed with enchanted player's creature must \
         still trigger via last-known-information; stack={on_stack}, pending={in_pending}"
    );
}

/// Negative case: Curse co-departs but the dying creature belongs to a DIFFERENT
/// player (not the enchanted player). The trigger must NOT fire.
#[test]
fn curse_co_departed_does_not_trigger_for_non_enchanted_players_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Curse of Clinging Webs", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE_DIES_ORACLE);
        builder.id()
    };

    // P0's creature (NOT the enchanted player P1).
    let creature_id = scenario.add_creature(P0, "Elvish Mystic", 1, 1).id();

    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();

    // Attach curse to P1 (enchanted player).
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    // Board wipe: both die simultaneously.
    {
        let curse_obj = runner.state_mut().objects.get_mut(&curse_id).unwrap();
        if !curse_obj
            .card_types
            .core_types
            .contains(&CoreType::Creature)
        {
            curse_obj.card_types.core_types.push(CoreType::Creature);
        }
        curse_obj.toughness = Some(1);
        curse_obj.base_toughness = Some(1);
        curse_obj.damage_marked = 1;
    }
    {
        let creature_obj = runner.state_mut().objects.get_mut(&creature_id).unwrap();
        creature_obj.damage_marked = 1;
    }

    let mut sba_events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut sba_events);

    assert_eq!(runner.state().objects[&curse_id].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&creature_id].zone, Zone::Graveyard);

    process_triggers(runner.state_mut(), &sba_events);
    drain_order_triggers_with_identity(runner.state_mut());

    let on_stack = stack_triggers_from(&runner, curse_id);
    let in_pending = pending_triggers_from(&runner, curse_id);
    let total = on_stack + in_pending;

    assert_eq!(
        total, 0,
        "Curse must NOT trigger for non-enchanted player's creature even in co-departure; \
         stack={on_stack}, pending={in_pending}"
    );
}
