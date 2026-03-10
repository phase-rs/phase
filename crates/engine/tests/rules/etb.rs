#![allow(unused_imports)]
use super::*;

use std::collections::HashMap;

use engine::types::game_state::StackEntryKind;
use engine::types::identifiers::CardId;
use engine::types::triggers::TriggerMode;

/// CR 603.6a: ChangesZone trigger fires when a creature enters the battlefield.
///
/// A permanent with a ChangesZone trigger (configured for ETB) fires when
/// another creature enters the battlefield via zone transition.
#[test]
fn etb_changes_zone_trigger_fires_on_zone_change() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Soul Warden on battlefield with ChangesZone trigger (watches zone transitions)
    let mut builder = scenario.add_creature(P0, "Soul Warden", 1, 1);
    builder.with_trigger(TriggerMode::ChangesZone);

    // A creature in P0's hand to cast
    let bear_builder = scenario.add_creature_to_hand(P0, "Grizzly Bears", 2, 2);
    let bear_id = bear_builder.id();

    let mut runner = scenario.build();

    // Cast the bear
    let bear_card_id = runner.state().objects[&bear_id].card_id;
    let _cast_result = runner
        .act(GameAction::CastSpell {
            card_id: bear_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    // After casting, the ChangesZone trigger fires on Hand->Stack transition.
    // This means the stack has both the spell AND the trigger.
    // The trigger being on the stack proves the trigger system detected the zone change.
    let stack_after_cast = runner.state().stack.len();
    assert!(
        stack_after_cast >= 2,
        "Stack should have spell + triggered ability after cast, got {}",
        stack_after_cast
    );

    // Verify there's a TriggeredAbility on the stack
    let has_trigger = runner.state().stack.iter().any(|entry| {
        matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
    });
    assert!(
        has_trigger,
        "Stack should contain a TriggeredAbility from the ChangesZone trigger"
    );

    // Now drain the entire stack (resolve trigger + creature spell)
    for _ in 0..20 {
        if runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    // Bear should be on the battlefield after everything resolves
    assert_eq!(
        runner.state().objects[&bear_id].zone,
        Zone::Battlefield,
        "Bear should be on the battlefield after all stack entries resolve"
    );
}

/// CR 603.6a: Multiple ChangesZone triggers fire when another creature changes zones.
///
/// Two permanents with ChangesZone triggers on the battlefield. When a third creature
/// is cast (Hand->Stack zone change), both triggers fire.
#[test]
fn multiple_changes_zone_triggers_fire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two creatures with ChangesZone triggers
    let mut w1 = scenario.add_creature(P0, "Soul Warden", 1, 1);
    w1.with_trigger(TriggerMode::ChangesZone);

    let mut w2 = scenario.add_creature(P0, "Essence Warden", 1, 1);
    w2.with_trigger(TriggerMode::ChangesZone);

    // A creature to enter
    let elf_builder = scenario.add_creature_to_hand(P0, "Llanowar Elves", 1, 1);
    let elf_id = elf_builder.id();

    let mut runner = scenario.build();

    // Cast the elf
    let elf_card_id = runner.state().objects[&elf_id].card_id;
    runner
        .act(GameAction::CastSpell {
            card_id: elf_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    // After casting, the Hand->Stack zone change fires both ChangesZone triggers.
    // Stack should have: elf spell + 2 triggered abilities = 3 entries.
    let stack_after_cast = runner.state().stack.len();
    assert!(
        stack_after_cast >= 3,
        "Stack should have spell + 2 triggered abilities, got {}",
        stack_after_cast
    );

    // Count triggered abilities on stack
    let trigger_count = runner
        .state()
        .stack
        .iter()
        .filter(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. }))
        .count();
    assert_eq!(
        trigger_count, 2,
        "Both ChangesZone triggers should have fired"
    );
}

/// CR 603.3: Triggered abilities go on the stack and can be responded to.
///
/// When a trigger fires, it is placed on the stack and the appropriate player
/// receives priority before it resolves.
#[test]
fn trigger_goes_on_stack_with_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Creature with ChangesZone trigger
    let mut warden = scenario.add_creature(P0, "Soul Warden", 1, 1);
    warden.with_trigger(TriggerMode::ChangesZone);

    // A creature to cast
    let bear_builder = scenario.add_creature_to_hand(P0, "Grizzly Bears", 2, 2);
    let bear_id = bear_builder.id();

    let mut runner = scenario.build();

    // Cast the bear
    let bear_card_id = runner.state().objects[&bear_id].card_id;
    runner
        .act(GameAction::CastSpell {
            card_id: bear_card_id,
            targets: vec![],
        })
        .expect("cast should succeed");

    // After casting, there should be a triggered ability on the stack.
    // The engine should be waiting for priority (so the player can respond).
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "After trigger placed on stack, should be waiting for priority, got {:?}",
        state.waiting_for
    );

    // The trigger is on the stack -- it hasn't resolved yet.
    // The player can respond (cast instants, activate abilities) before it resolves.
    let has_trigger_on_stack = state.stack.iter().any(|entry| {
        matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
    });
    assert!(
        has_trigger_on_stack,
        "Triggered ability should be on the stack awaiting resolution"
    );
}
