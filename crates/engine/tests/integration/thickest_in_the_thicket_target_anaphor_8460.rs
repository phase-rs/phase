//! Regression for GitHub issue #8460 — "where X is that creature's power" on a
//! clause that announces its OWN target creature.
//!
//! Oracle (Thickest in the Thicket): "When this enchantment enters, put X +1/+1
//! counters on target creature, where X is that creature's power."
//!
//! The bug: `parse_event_context_refs` lowers the context-free phrase "that
//! creature's power" to `QuantityRef::Power { scope: ObjectScope::CostPaidObject }`
//! — the CR 608.2k cost/trigger-referent sense it correctly carries on
//! Hamletback Goliath ("whenever another creature enters ... where X is that
//! creature's power") and Shadowheart, Dark Justiciar (a sacrifice cost). On a
//! clause that announces its own "target creature" there is no cost referent and
//! no earlier instruction, so CR 115.1 makes the announced target the antecedent.
//!
//! At runtime `ObjectScope::CostPaidObject` walks cost-paid object -> effect
//! context object -> TRIGGER-EVENT SOURCE. For an ETB trigger that third slot is
//! the entering permanent itself, so X read Thickest's own power: 0 for an
//! ordinary enchantment (the reported "every creature effectively gets +0/+0"),
//! and 4 while Bello, Bard of the Brambles animated it as a 4/4 (the reporter's
//! "oddly, this works if you target Thickest itself" — it coincided with the
//! right answer only because target and source were the same object).
//!
//! These tests parse the real Oracle text through the production parser path and
//! drive the full cast pipeline, so they exercise parser + runtime end-to-end.
//! Reverting the lowering-seam rebind returns the pre-fix `CostPaidObject` scope
//! and every assertion below drops to 0 counters.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;

const THICKEST_IN_THE_THICKET: &str =
    "When this enchantment enters, put X +1/+1 counters on target creature, \
where X is that creature's power.\n\
At the beginning of your end step, draw two cards if you control the creature \
with the greatest power or tied for the greatest power.";

const SOULS_MIGHT: &str =
    "Put X +1/+1 counters on target creature, where X is that creature's power.";

/// CR 115.1 + CR 608.2c: the ETB trigger's "that creature" is the creature it
/// announced as a target, so a targeted 3/3 gets exactly 3 counters.
///
/// Two independent discriminators sit in this one board:
///   * 0 counters means X still resolved through `CostPaidObject`, whose ETB
///     fallback is the entering enchantment (power 0) — the reported bug.
///   * 7 counters would mean X read the untargeted decoy rather than the
///     announced target.
#[test]
fn thickest_in_the_thicket_counters_equal_the_targeted_creatures_power() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let enchantment = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Thickest in the Thicket",
            false,
            THICKEST_IN_THE_THICKET,
        )
        .as_enchantment()
        .id();
    let target = scenario.add_creature(P0, "Targeted Beast", 3, 3).id();
    // Power 7, never targeted — discriminates "the announced target" from "some
    // creature on the battlefield".
    let decoy = scenario.add_creature(P1, "Untargeted Ogre", 7, 7).id();

    let mut runner = scenario.build();
    let outcome = runner.cast(enchantment).target_object(target).resolve();

    outcome.assert_counters(target, CounterType::Plus1Plus1, 3);
    outcome.assert_counters(decoy, CounterType::Plus1Plus1, 0);
}

/// CR 608.2h: X is calculated once, as the ability resolves (the card's own
/// ruling: "The value of X is calculated only once, as Thickest in the Thicket's
/// first ability resolves"). A creature already carrying counters therefore
/// contributes its CURRENT power, not its printed power — a 2/2 with three
/// +1/+1 counters is a 5/5 and gains five more counters.
#[test]
fn thickest_in_the_thicket_reads_the_targets_current_power() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let enchantment = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Thickest in the Thicket",
            false,
            THICKEST_IN_THE_THICKET,
        )
        .as_enchantment()
        .id();
    let target = scenario
        .add_creature(P0, "Counter-Laden Beast", 2, 2)
        .with_plus_counters(3)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(enchantment).target_object(target).resolve();

    // 3 pre-existing + 5 (its current power) = 8.
    outcome.assert_counters(target, CounterType::Plus1Plus1, 8);
}

/// The same clause with no trigger at all (Soul's Might, a plain sorcery). It
/// shares the exact effect node — `PutCounter { count: Power{..}, target:
/// creature }` — and isolates the rebind from any trigger-event machinery: with
/// no cost referent, no effect-context object AND no trigger event, the pre-fix
/// `CostPaidObject` ladder had nothing at all to read and produced 0.
#[test]
fn souls_might_counters_equal_the_targeted_creatures_power() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Soul's Might", false, SOULS_MIGHT)
        .id();
    let target = scenario.add_creature(P0, "Mighty Beast", 4, 4).id();
    let decoy = scenario.add_creature(P1, "Untargeted Ogre", 9, 9).id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(target).resolve();

    outcome.assert_counters(target, CounterType::Plus1Plus1, 4);
    outcome.assert_counters(decoy, CounterType::Plus1Plus1, 0);
}

/// The reporter's own accidental diagnostic, made discriminating. They noticed
/// the card "will work if you target Thickest in the Thicket itself while Bello,
/// Bard of the Brambles is making it a 4/4 creature, giving it +4/+4" — i.e. the
/// count was right exactly when the announced target and the ability's source
/// happened to be the same object.
///
/// This models that board (an animated Thickest — same verbatim Oracle text,
/// entering as a 4/4 enchantment creature) but targets a DIFFERENT creature, so
/// source power (4) and target power (3) disagree. Under the pre-fix
/// `CostPaidObject` scope the ETB trigger-event-source fallback reads the
/// entering permanent and yields 4; the announced target's power is 3.
#[test]
fn animated_thickest_reads_the_target_not_the_animated_source() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // CR 205.1b + CR 613.4b: an enchantment that is also a 4/4 creature — the
    // board Bello, Bard of the Brambles produces for a mana-value-4 enchantment.
    let enchantment = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Thickest in the Thicket",
            4,
            4,
            THICKEST_IN_THE_THICKET,
        )
        .as_enchantment()
        .as_creature()
        .id();
    let target = scenario.add_creature(P0, "Targeted Beast", 3, 3).id();

    let mut runner = scenario.build();
    let outcome = runner.cast(enchantment).target_object(target).resolve();

    outcome.assert_counters(target, CounterType::Plus1Plus1, 3);
    // The animated source is not the antecedent and receives nothing.
    outcome.assert_counters(enchantment, CounterType::Plus1Plus1, 0);
}
