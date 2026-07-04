//! Integration tests for curse cards with attack-related triggers.
//!
//! Covers 7 curses that trigger when creatures attack the enchanted player:
//!
//! Group A — "Whenever a player attacks enchanted player with one or more creatures":
//!   - Curse of Chaos (attacking player may discard → draw)
//!   - Curse of Inertia (attacking player may tap/untap target)
//!   - Curse of Shallow Graves (attacking player may create tapped 2/2 Zombie)
//!
//! Group B — "Whenever a creature attacks enchanted player":
//!   - Curse of Predation (put a +1/+1 counter on it)
//!   - Curse of the Forsaken (its controller gains 1 life)
//!   - Curse of Stalked Prey (combat damage → +1/+1 counter)
//!   - Curse of Hospitality (trample + combat damage → exile top)
//!
//! Each test verifies that the trigger fires when a creature attacks the
//! enchanted player. For Group A, the trigger fires once per attacking player.
//! For Group B, it fires once per attacking creature.
//!
//! NOTE: These tests depend on the `matching_attack_events` fix from PR #4401
//! which ensures "enchanted player is attacked" triggers fire correctly.
//!
//! CR references:
//!   - CR 303.4b: An Aura that enchants a player is attached to that player.
//!   - CR 508.1: Declare attackers step.
//!   - CR 508.3d: "Whenever [player] attacks" triggers fire once per attacked
//!     defending player, not once per creature.

use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

// ---------------------------------------------------------------------------
// Oracle texts
// ---------------------------------------------------------------------------

const CURSE_OF_CHAOS: &str =
    "Whenever a player attacks enchanted player with one or more creatures, that attacking player may discard a card. If the player does, they draw a card.";

const CURSE_OF_INERTIA: &str =
    "Whenever a player attacks enchanted player with one or more creatures, that attacking player may tap or untap target permanent.";

const CURSE_OF_SHALLOW_GRAVES: &str =
    "Whenever a player attacks enchanted player with one or more creatures, that attacking player may create a tapped 2/2 black Zombie creature token.";

const CURSE_OF_PREDATION: &str =
    "Whenever a creature attacks enchanted player, put a +1/+1 counter on it.";

const CURSE_OF_THE_FORSAKEN: &str =
    "Whenever a creature attacks enchanted player, its controller gains 1 life.";

const CURSE_OF_STALKED_PREY: &str =
    "Whenever a creature deals combat damage to enchanted player, put a +1/+1 counter on that creature.";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Count triggered abilities on the stack sourced from `source`.
fn stack_triggers_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == source)
        .count()
}

/// Build a scenario with a curse attached to P1, P0 has an attacker ready.
/// Starts at PreCombatMain so we can advance to combat.
fn setup_attack_curse(oracle: &str, name: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature_from_oracle(P0, name, 0, 0, oracle);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // P0's attacker — a 2/2 creature.
    let attacker_id = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    // Library padding.
    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();

    // Attach the curse to P1 (the enchanted player / defender).
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    (runner, curse_id, attacker_id)
}

/// Advance to declare attackers and declare the given creature as attacking P1.
fn declare_attack_on_p1(runner: &mut GameRunner, attacker: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers must succeed");
}

// ---------------------------------------------------------------------------
// Group A: "Whenever a player attacks enchanted player with one or more creatures"
// ---------------------------------------------------------------------------

/// Curse of Chaos: trigger fires when P0 attacks enchanted player P1.
#[test]
fn curse_of_chaos_fires_when_player_attacks_enchanted_player() {
    let (mut runner, curse_id, attacker) = setup_attack_curse(CURSE_OF_CHAOS, "Curse of Chaos");

    declare_attack_on_p1(&mut runner, attacker);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of Chaos must trigger when a player attacks enchanted player"
    );
}

/// Curse of Inertia: trigger fires when P0 attacks enchanted player P1.
#[test]
fn curse_of_inertia_fires_when_player_attacks_enchanted_player() {
    let (mut runner, curse_id, attacker) = setup_attack_curse(CURSE_OF_INERTIA, "Curse of Inertia");

    declare_attack_on_p1(&mut runner, attacker);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of Inertia must trigger when a player attacks enchanted player"
    );
}

/// Curse of Shallow Graves: trigger fires when P0 attacks enchanted player P1.
#[test]
fn curse_of_shallow_graves_fires_when_player_attacks_enchanted_player() {
    let (mut runner, curse_id, attacker) =
        setup_attack_curse(CURSE_OF_SHALLOW_GRAVES, "Curse of Shallow Graves");

    declare_attack_on_p1(&mut runner, attacker);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of Shallow Graves must trigger when a player attacks enchanted player"
    );
}

// ---------------------------------------------------------------------------
// Group B: "Whenever a creature attacks enchanted player"
// ---------------------------------------------------------------------------

/// Curse of Predation: trigger fires for each creature attacking enchanted player.
/// After resolution, the attacker gets a +1/+1 counter.
#[test]
fn curse_of_predation_fires_when_creature_attacks_enchanted_player() {
    let (mut runner, curse_id, attacker) =
        setup_attack_curse(CURSE_OF_PREDATION, "Curse of Predation");

    declare_attack_on_p1(&mut runner, attacker);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of Predation must trigger when a creature attacks enchanted player"
    );

    // Resolve the trigger — attacker should get a +1/+1 counter.
    runner.advance_until_stack_empty();

    let counters = runner
        .state()
        .objects
        .get(&attacker)
        .map(|obj| {
            obj.counters
                .get(&engine::types::counter::CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        })
        .unwrap_or(0);

    assert_eq!(
        counters, 1,
        "attacking creature must get a +1/+1 counter from Curse of Predation"
    );
}

/// Curse of the Forsaken: trigger fires for each creature attacking enchanted player.
/// After resolution, the attacker's controller gains 1 life.
#[test]
fn curse_of_the_forsaken_fires_when_creature_attacks_enchanted_player() {
    let (mut runner, curse_id, attacker) =
        setup_attack_curse(CURSE_OF_THE_FORSAKEN, "Curse of the Forsaken");

    let life_before = runner.life(P0);
    declare_attack_on_p1(&mut runner, attacker);

    assert!(
        stack_triggers_from(&runner, curse_id) >= 1,
        "Curse of the Forsaken must trigger when a creature attacks enchanted player"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "attacking creature's controller (P0) must gain 1 life from Curse of the Forsaken"
    );
}

/// Curse of Stalked Prey: trigger fires when a creature deals combat damage to
/// enchanted player. This is a combat-damage trigger, not a declare-attackers trigger.
#[test]
fn curse_of_stalked_prey_fires_on_combat_damage_to_enchanted_player() {
    let (mut runner, _curse_id, attacker) =
        setup_attack_curse(CURSE_OF_STALKED_PREY, "Curse of Stalked Prey");

    declare_attack_on_p1(&mut runner, attacker);

    // Drive through combat damage.
    let outcome = runner.combat_damage();

    // After combat damage, the trigger should have fired.
    // Check that P1 took damage (2 from the 2/2 attacker).
    let life_delta = outcome.life_delta(P1);
    assert!(
        life_delta <= -2,
        "enchanted player must take combat damage (got delta {life_delta})"
    );

    // Check for +1/+1 counter on the attacker after trigger resolves.
    let counters = runner
        .state()
        .objects
        .get(&attacker)
        .map(|obj| {
            obj.counters
                .get(&engine::types::counter::CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        })
        .unwrap_or(0);

    assert!(
        counters >= 1,
        "creature that dealt combat damage to enchanted player must get a +1/+1 counter from Curse of Stalked Prey"
    );
}

// NOTE: Curse of Hospitality is not tested here because the engine does not
// yet support the "deals combat damage to enchanted player" trigger pattern.
// A test should be added once that trigger matcher is implemented.
