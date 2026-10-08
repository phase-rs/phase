//! DynQty subgroup A — damage-modification replacement runtime guards
//! (CR 614.1a). PERMANENT regression tests backing the coverage flip of two
//! cards to `supported:True`.
//!
//! * Neriv, Heart of the Storm — "If a creature you control that entered this
//!   turn would deal damage, it deals twice that much damage instead."
//!   (`DamageModification::Double`, source filter carries `EnteredThisTurn`).
//!   Proves BOTH the doubling AND the entered-this-turn gate: an
//!   entered-this-turn source doubles (2 → 4); a prior-turn source does not
//!   (stays 2). Reverting the "twice that much damage" → `Double` parser leaf
//!   (oracle_replacement.rs) drops the replacement entirely, so the source no
//!   longer doubles.
//!
//! * Fated Firepower — "...it deals that much damage plus an amount of damage
//!   equal to the number of fire counters on this enchantment instead."
//!   (`DamageModification::Plus { Ref(CountersOn fire) }`). Rules-correctness
//!   regression: with 3 fire counters, a 2-damage source deals 5 (2 + 3), NOT 3
//!   (the reverted `Plus{Fixed{1}}` "an" → 1 bug), NOT 2. Reverting the dynamic
//!   "plus an amount of damage equal to <quantity>" parser arm regresses this to
//!   3.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{DamageModification, ReplacementDefinition};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;

use super::rules::run_combat;

const NERIV_ORACLE: &str = "Flying\n\
    If a creature you control that entered this turn would deal damage, it deals twice that much \
    damage instead.";

const FATED_FIREPOWER_ORACLE: &str = "Flash\n\
    This enchantment enters with X fire counters on it.\n\
    If a source you control would deal damage to an opponent or a permanent an opponent controls, \
    it deals that much damage plus an amount of damage equal to the number of fire counters on \
    this enchantment instead.";

fn parse_card(oracle: &str, name: &str, types: &[&str]) -> ParsedAbilities {
    let types: Vec<String> = types.iter().map(|s| (*s).to_string()).collect();
    parse_oracle_text(oracle, name, &[], &types, &[])
}

/// The single damage-modification replacement on the card. `.expect` here is a
/// reach-guard: reverting the parser arm under test drops the replacement, so
/// this panics (the test fails) — the doubling/offset behavior below can only be
/// asserted once the replacement is present.
fn damage_modification_replacement(
    oracle: &str,
    name: &str,
    types: &[&str],
) -> ReplacementDefinition {
    parse_card(oracle, name, types)
        .replacements
        .into_iter()
        .find(|r| r.damage_modification.is_some())
        .expect("expected a damage-modification replacement")
}

// ── Neriv, Heart of the Storm ───────────────────────────────────────────────

/// Neriv (a): an entered-this-turn source you control deals 2 combat damage →
/// DOUBLED to 4 via `DamageModification::Double`, gated on the `EnteredThisTurn`
/// source filter. Revert surface: revert the "twice that much damage" → Double
/// parser leaf and the replacement is dropped, so the source no longer doubles.
#[test]
fn neriv_entered_this_turn_source_doubles() {
    let repl = damage_modification_replacement(
        NERIV_ORACLE,
        "Neriv, Heart of the Storm",
        &["Legendary", "Creature", "Dragon"],
    );
    assert_eq!(repl.damage_modification, Some(DamageModification::Double));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Attacker entered THIS turn (summoning-sick) but has haste so it can attack;
    // `entered_battlefield_turn == turn_number` ⇒ EnteredThisTurn TRUE.
    let fresh = scenario
        .add_creature(P0, "Fresh Raider", 2, 2)
        .with_summoning_sickness()
        .haste()
        .id();
    let wall = scenario.add_creature(P1, "Stone Wall", 0, 10).id();
    // Neriv holds the replacement but does not attack — the doubler must apply to
    // ANY entered-this-turn creature you control, not only its own source.
    scenario
        .add_creature(P0, "Neriv, Heart of the Storm", 1, 1)
        .with_replacement_definition(repl);

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![fresh], vec![(wall, fresh)]);

    let marked = runner.state().objects.get(&wall).unwrap().damage_marked;
    assert_eq!(
        marked, 4,
        "CR 614.1a + CR 701.10g: an entered-this-turn source's 2 damage must double to 4"
    );
}

/// Neriv (b): DISCRIMINATING for the `EnteredThisTurn` gate — a source that
/// entered a PRIOR turn is NOT doubled; its 2 combat damage stays 2. Revert the
/// gate (source filter loses `EnteredThisTurn`) and this doubles to 4, failing.
#[test]
fn neriv_prior_turn_source_not_doubled() {
    let repl = damage_modification_replacement(
        NERIV_ORACLE,
        "Neriv, Heart of the Storm",
        &["Legendary", "Creature", "Dragon"],
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Plain add_creature ⇒ entered_battlefield_turn = turn_number - 1 (prior
    // turn), not summoning-sick ⇒ EnteredThisTurn FALSE.
    let veteran = scenario.add_creature(P0, "Veteran Raider", 2, 2).id();
    let wall = scenario.add_creature(P1, "Stone Wall", 0, 10).id();
    scenario
        .add_creature(P0, "Neriv, Heart of the Storm", 1, 1)
        .with_replacement_definition(repl);

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![veteran], vec![(wall, veteran)]);

    let marked = runner.state().objects.get(&wall).unwrap().damage_marked;
    assert_eq!(
        marked, 2,
        "a prior-turn source must not be doubled (EnteredThisTurn source filter)"
    );
}

// ── Fated Firepower ─────────────────────────────────────────────────────────

/// Fated Firepower: rules-correctness regression. With 3 fire counters on the
/// enchantment, a 2-power source you control dealing combat damage to a permanent
/// an opponent controls deals 2 + 3 = 5 — `Plus { Ref(CountersOn fire) }` resolved
/// against the replacement source (the enchantment). Revert surface: revert the
/// dynamic "plus an amount of damage equal to <quantity>" parser arm and the
/// offset regresses to `Plus{Fixed{1}}` (2 + 1 = 3).
#[test]
fn fated_firepower_offset_scales_with_fire_counters() {
    let repl = damage_modification_replacement(
        FATED_FIREPOWER_ORACLE,
        "Fated Firepower",
        &["Enchantment"],
    );
    assert!(
        matches!(
            repl.damage_modification,
            Some(DamageModification::Plus { .. })
        ),
        "FF must carry an additive Plus modification (revert → Plus{{Fixed{{1}}}} is still Plus, \
         so the runtime magnitude below is the discriminator)"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // 2-power source you control (prior turn ⇒ can attack).
    let source = scenario.add_creature(P0, "Ember Striker", 2, 2).id();
    // A permanent an opponent controls (matches FF's target filter).
    let wall = scenario.add_creature(P1, "Stone Wall", 0, 10).id();
    // Fated Firepower with 3 fire counters holds the replacement; CountersOn
    // resolves against the replacement source = this enchantment.
    let ff = scenario
        .add_creature(P0, "Fated Firepower", 0, 0)
        .as_enchantment()
        .with_replacement_definition(repl)
        .id();
    scenario.with_counter(ff, CounterType::Generic("fire".to_string()), 3);

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![source], vec![(wall, source)]);

    let marked = runner.state().objects.get(&wall).unwrap().damage_marked;
    assert_eq!(
        marked, 5,
        "CR 614.1a + CR 120: 2 base + 3 fire counters = 5 (revert → Plus{{Fixed{{1}}}} = 3)"
    );
}
