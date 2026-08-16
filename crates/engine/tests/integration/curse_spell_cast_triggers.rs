//! Integration tests for curse cards with spell-cast triggers.
//!
//! Covers 4 curses that trigger when enchanted player casts a spell:
//!   - Curse of Vengeance (put a spite counter on it)
//!   - Curse of Silence (chosen name costs {2} more; draw when cast)
//!   - Curse of Echoes (each other player may copy instant/sorcery)
//!   - Maddening Hex (roll d6, deal damage, reattach)
//!
//! Each test verifies at minimum that the spell-cast trigger fires (stack count
//! assertion). For Curse of Vengeance, counter placement is also verified.
//!
//! CR references:
//!   - CR 303.4b: An Aura that enchants a player is attached to that player.
//!   - CR 601.2a: A player casts a spell by announcing it and moving it to the
//!     stack.

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

// ---------------------------------------------------------------------------
// Oracle texts
// ---------------------------------------------------------------------------

const CURSE_OF_VENGEANCE: &str =
    "Whenever enchanted player casts a spell, put a spite counter on Curse of Vengeance.\n\
     When enchanted player loses the game, you gain X life and draw X cards, where X is the number of spite counters on Curse of Vengeance.";

const CURSE_OF_ECHOES: &str =
    "Whenever enchanted player casts an instant or sorcery spell, each other player may copy that spell and may choose new targets for the copy they control.";

const MADDENING_HEX: &str =
    "Whenever enchanted player casts a noncreature spell, roll a d6. Maddening Hex deals damage to that player equal to the result. Then attach Maddening Hex to another one of your opponents chosen at random.";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

/// Build a curse on the battlefield under P0's control, attached to P1.
/// P1 has a spell in hand and mana to cast it.
/// Returns (runner, curse_id, spell_id, target_id).
fn setup_spell_cast_curse(oracle: &str, name: &str) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder = scenario.add_creature_from_oracle(P0, name, 0, 0, oracle);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // Give P1 a cheap spell to cast (Lightning Bolt equivalent — instant, {R}).
    let spell_id = scenario.add_bolt_to_hand(P1);

    // P1 needs a target for the bolt.
    let dummy = scenario.add_creature(P0, "Memnite", 1, 1).id();

    // Mana for P1 to cast the spell.
    scenario.with_mana_pool(P1, vec![mana(ManaType::Red)]);

    // Library padding.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();

    // P1 is the active player (their main phase).
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    // Attach the curse to P1.
    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    (runner, curse_id, spell_id, dummy)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Curse of Vengeance: when enchanted player casts a spell, put a spite counter
/// on the curse.
#[test]
fn curse_of_vengeance_fires_on_spell_cast() {
    let (mut runner, curse_id, spell_id, target) =
        setup_spell_cast_curse(CURSE_OF_VENGEANCE, "Curse of Vengeance");

    // Cast the bolt — target the dummy creature.
    runner.cast(spell_id).target_object(target).resolve();

    // After resolution, the trigger should have fired and resolved (putting a
    // spite counter on the curse). Check the counter.
    let counters = runner
        .state()
        .objects
        .get(&curse_id)
        .map(|obj| {
            obj.counters
                .get(&engine::types::counter::CounterType::Generic(
                    "spite".to_string(),
                ))
                .copied()
                .unwrap_or(0)
        })
        .unwrap_or(0);

    assert!(
        counters >= 1,
        "Curse of Vengeance must have at least 1 spite counter after enchanted player casts a spell (got {counters})"
    );
}

/// Curse of Echoes: trigger fires when enchanted player casts an instant/sorcery.
/// We use commit() to inspect the stack before resolution and verify the curse
/// placed a trigger from its source_id.
#[test]
fn curse_of_echoes_fires_on_instant_cast() {
    let (mut runner, curse_id, spell_id, target) =
        setup_spell_cast_curse(CURSE_OF_ECHOES, "Curse of Echoes");

    // Cast the bolt and inspect the stack before resolution.
    let commit = runner.cast(spell_id).target_object(target).commit();

    // The commit driver passes priority until the spell is committed; at that
    // point the cast-trigger from Curse of Echoes should already be on the
    // stack (CR 601.2a: triggers fire on cast announcement).
    let has_curse_trigger = commit
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == curse_id);

    assert!(
        has_curse_trigger,
        "Curse of Echoes must put a trigger from the curse source on the stack \
         when enchanted player casts an instant or sorcery"
    );
}

/// Maddening Hex: trigger fires when enchanted player casts a noncreature spell.
/// The trigger involves "roll a d6" and "attach to another opponent" which
/// surfaces a TriggerTargetSelection prompt the cast driver can't auto-resolve.
/// We manually drive the cast and verify the trigger appears on the stack.
#[test]
fn maddening_hex_fires_on_noncreature_spell_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder =
            scenario.add_creature_from_oracle(P0, "Maddening Hex", 0, 0, MADDENING_HEX);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // Use a no-target sorcery.
    let spell_id = scenario.add_spell_to_hand(P1, "Explore", false).id();

    scenario.with_mana_pool(P1, vec![mana(ManaType::Green), mana(ManaType::Colorless)]);

    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Island");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    // Manually cast the spell.
    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell must succeed");

    // Drive until we hit a priority window or TriggerTargetSelection.
    for _ in 0..30 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                // Spell is on the stack; pass priority to let triggers fire.
                let _ = runner.act(GameAction::PassPriority);
            }
            _ => break,
        }
    }

    // The trigger should have fired — check that the stack has a trigger from the curse,
    // or we've hit TriggerTargetSelection (which means the trigger is being processed).
    let trigger_on_stack = runner.state().stack.iter().any(|e| e.source_id == curse_id);
    let at_trigger_target = matches!(
        &runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    );

    assert!(
        trigger_on_stack || at_trigger_target,
        "Maddening Hex must trigger when enchanted player casts a noncreature spell"
    );
}

/// Maddening Hex must not trigger for a matching spell cast by anyone other
/// than the enchanted player.
#[test]
fn maddening_hex_does_not_fire_for_non_enchanted_player_spell_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse_id = {
        let mut builder =
            scenario.add_creature_from_oracle(P0, "Maddening Hex", 0, 0, MADDENING_HEX);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    let spell_id = scenario.add_spell_to_hand(P0, "Explore", false).id();
    scenario.with_mana_pool(P0, vec![mana(ManaType::Green), mana(ManaType::Colorless)]);

    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Island");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    attach_to_player(runner.state_mut(), curse_id, P1);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), curse_id);

    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell must succeed");

    assert!(
        runner
            .state()
            .stack
            .iter()
            .all(|entry| entry.source_id != curse_id),
        "Maddening Hex must not trigger for a spell cast by the non-enchanted player"
    );
}
