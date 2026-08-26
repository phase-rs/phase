//! Issue #4744 — Curse of Predation: "Whenever a creature attacks enchanted
//! player, put a +1/+1 counter on it."
//!
//! The "enchanted player" defender scope must bind to the Aura's attached
//! player (parsed as `valid_target = AttachedTo`). Before the fix the trigger
//! had no defender scope and fired on every attack anywhere; the risk the fix
//! introduces is the opposite failure — if enchant-player attachment doesn't
//! populate `attached_to` as a player, the trigger would never fire. This test
//! discriminates both directions:
//!   1. A creature attacks the ENCHANTED player  → the attacker gets a counter.
//!   2. A creature attacks a DIFFERENT player     → no counter is placed.
//!
//! CR references:
//!   - CR 303.4e: An Aura's controller is separate from the enchanted player;
//!     the trigger's defender scope is the attached player, resolved at runtime
//!     via `TargetFilter::AttachedTo` (`game/trigger_matchers.rs`).
//!   - CR 508.1a: The active player chooses which creatures will attack.

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

const CURSE_OF_PREDATION_ORACLE: &str = "Enchant player\n\
     Whenever a creature attacks enchanted player, put a +1/+1 counter on it.";

/// +1/+1 counters on `id`.
fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

/// Curse of Predation enchants P1; a creature P0 controls attacks P1 (the
/// enchanted player). The attacking creature must gain a +1/+1 counter.
#[test]
fn curse_of_predation_counter_placed_when_enchanted_player_attacked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Curse of Predation", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE_OF_PREDATION_ORACLE);
        builder.id()
    };
    // P0 attacks the enchanted player (P1) with this creature.
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    assert_eq!(
        plus_one_counters(&runner, attacker),
        0,
        "no counter before combat"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        plus_one_counters(&runner, attacker),
        1,
        "attacking the enchanted player must place a +1/+1 counter on the attacker"
    );
}

/// Discriminator: Curse of Predation enchants P1, but a creature attacks P0 (a
/// DIFFERENT player). The trigger must NOT fire — no counter is placed. Without
/// the defender scope this test fails (the trigger would fire on any attack);
/// if the fix over-corrected and `AttachedTo` never resolved, the positive test
/// above would fail instead. Together they pin both failure modes.
#[test]
fn curse_of_predation_no_counter_when_non_enchanted_player_attacked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature(P0, "Curse of Predation", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.from_oracle_text(CURSE_OF_PREDATION_ORACLE);
        builder.id()
    };
    // P1 attacks P0 — P0 is NOT the enchanted player (P1 is).
    let attacker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        plus_one_counters(&runner, attacker),
        0,
        "attacking a non-enchanted player must NOT place a counter (defender scope)"
    );
}
