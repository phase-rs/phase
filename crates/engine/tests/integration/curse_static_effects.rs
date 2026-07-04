//! Integration tests for curse cards with static or replacement effects.
//!
//! Covers 3 curses with continuous/replacement effects:
//!   - Curse of Exhaustion (enchanted player can't cast more than one spell each turn)
//!   - Overwhelming Splendor (creatures enchanted player controls lose all abilities
//!     and have base power and toughness 1/1; can't activate nonmana abilities)
//!   - Curse of Bloodletting (if a source would deal damage to enchanted player,
//!     it deals double that damage instead)
//!
//! Each test verifies the static/replacement effect is active while the curse is
//! attached to the enchanted player.
//!
//! CR references:
//!   - CR 303.4b: An Aura that enchants a player is attached to that player.
//!   - CR 613.4c: Layer 7c — power/toughness modifications.
//!   - CR 611.3: A continuous effect ends when its source leaves.

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

// ---------------------------------------------------------------------------
// Oracle texts
// ---------------------------------------------------------------------------

const CURSE_OF_EXHAUSTION: &str = "Enchanted player can't cast more than one spell each turn.";

const OVERWHELMING_SPLENDOR: &str =
    "Creatures enchanted player controls lose all abilities and have base power and toughness 1/1.\n\
     Enchanted player can't activate nonmana abilities of permanents they control.";

const CURSE_OF_BLOODLETTING: &str =
    "If a source would deal damage to enchanted player, it deals double that damage to that player instead.";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Overwhelming Splendor: creatures enchanted player controls have base P/T 1/1.
#[test]
fn overwhelming_splendor_sets_base_pt_to_1_1() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Overwhelming Splendor",
            0,
            0,
            OVERWHELMING_SPLENDOR,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // Enchanted player's creature — a 4/4 Angel.
    let foe = scenario.add_creature(P1, "Serra Angel", 4, 4).id();

    // Controller's own creature — outside the filter.
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse, P1);

    // Enchanted player's creature becomes 1/1.
    assert_eq!(
        effective_pt(&mut runner, foe),
        (1, 1),
        "enchanted player's creature must have base P/T 1/1 under Overwhelming Splendor"
    );

    // Controller's creature is unaffected.
    assert_eq!(
        effective_pt(&mut runner, ally),
        (2, 2),
        "curse controller's creature must NOT be affected by Overwhelming Splendor"
    );
}

/// Overwhelming Splendor: effect ends when source leaves the battlefield.
#[test]
fn overwhelming_splendor_effect_ends_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Overwhelming Splendor",
            0,
            0,
            OVERWHELMING_SPLENDOR,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    let foe = scenario.add_creature(P1, "Serra Angel", 4, 4).id();

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse, P1);

    assert_eq!(
        effective_pt(&mut runner, foe),
        (1, 1),
        "baseline: creature is 1/1 while Overwhelming Splendor is present"
    );

    // Remove the curse.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != curse);
        state.objects.remove(&curse);
    }

    assert_eq!(
        effective_pt(&mut runner, foe),
        (4, 4),
        "creature reverts to 4/4 once Overwhelming Splendor is gone"
    );
}

/// Curse of Exhaustion: enchanted player can't cast more than one spell per turn.
/// We cast one spell as P1, then verify `can_cast_object_now` returns false for
/// a second spell — proving the PerTurnCastLimit static is enforced.
#[test]
fn curse_of_exhaustion_restricts_enchanted_player() {
    use engine::game::casting::can_cast_object_now;
    use engine::types::mana::{ManaCost, ManaType, ManaUnit};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse = {
        let mut builder =
            scenario.add_creature_from_oracle(P0, "Curse of Exhaustion", 0, 0, CURSE_OF_EXHAUSTION);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // P1 has two free instants.
    let spell_1 = scenario
        .add_spell_to_hand(P1, "Shock", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .from_oracle_text("Shock deals 2 damage to any target.")
        .id();
    let spell_2 = scenario
        .add_spell_to_hand(P1, "Lightning Bolt", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .from_oracle_text("Lightning Bolt deals 3 damage to any target.")
        .id();

    // Target for the first spell.
    let target = scenario.add_creature(P0, "Memnite", 1, 1).id();

    // Give P1 mana.
    let dummy = engine::types::identifiers::ObjectId(0);
    let mana = vec![
        ManaUnit::new(ManaType::Red, dummy, false, vec![]),
        ManaUnit::new(ManaType::Red, dummy, false, vec![]),
    ];
    scenario.with_mana_pool(P1, mana);

    // Library padding.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = engine::types::game_state::WaitingFor::Priority { player: P1 };

    attach_to_player(runner.state_mut(), curse, P1);
    evaluate_layers(runner.state_mut());

    // Before casting anything, P1 should be able to cast spell_2.
    assert!(
        can_cast_object_now(runner.state(), P1, spell_2),
        "P1 must be able to cast the second spell BEFORE the first cast"
    );

    // Cast the first spell.
    runner.cast(spell_1).target_object(target).resolve();

    // After casting one spell, P1 must NOT be able to cast the second.
    assert!(
        !can_cast_object_now(runner.state(), P1, spell_2),
        "Curse of Exhaustion must prevent enchanted player from casting a second spell this turn"
    );
}

/// Curse of Bloodletting: damage to enchanted player is doubled.
/// We test by dealing damage via a simple source and checking life loss.
#[test]
fn curse_of_bloodletting_doubles_damage_to_enchanted_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of Bloodletting",
            0,
            0,
            CURSE_OF_BLOODLETTING,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // P0 has a creature that will attack P1.
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    // Library padding.
    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    attach_to_player(runner.state_mut(), curse, P1);
    evaluate_layers(runner.state_mut());

    // Advance to combat and declare the attacker.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, engine::game::combat::AttackTarget::Player(P1))])
        .expect("DeclareAttackers must succeed");

    let life_before = runner.life(P1);

    // Drive through combat damage.
    runner.combat_damage();

    // With Curse of Bloodletting, 2 damage is doubled to 4.
    let life_after = runner.life(P1);
    let damage_taken = life_before - life_after;

    assert_eq!(
        damage_taken, 4,
        "Curse of Bloodletting must double damage to enchanted player: 2 → 4 (got {damage_taken})"
    );
}
