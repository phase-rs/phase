//! CR 614.6 + CR 614.15 — a CROSS-LINE "instead" self-replacement override is a
//! BRANCH of the ability it replaces, never an independent sibling ability.
//!
//! CR 614.6:  "If an event is replaced, it never happens. A modified event
//!             occurs instead."
//! CR 614.15: self-replacement effects "replace part or all of that spell or
//!             ability's own effect(s) … the text can be a separate ability,
//!             particularly when preceded by an ability word."
//!
//! CR 614.15 is the authority for this class: the override is printed as its own
//! ability-word LINE ("Corrupted — …", "Spell mastery — …"), but it replaces the
//! PREVIOUS printed line's effect. The parser has a cross-line binder for exactly
//! this (oracle.rs), but its gate recognized only WHOLE-clause overrides (a bare
//! trailing "instead"). It did not recognize the CR 614.15 PARTIAL forms —
//! "… instead of <N>" / "… instead of <phrase>" — nor an override whose condition
//! failed to lower. Those lines fell through and were emitted as INDEPENDENT
//! top-level abilities, so the engine performed the base effect AND the override.
//!
//! Oracle text below is verbatim from the full-pool export (never a paraphrase):
//! a paraphrase can take a different parser branch and leave the real card broken.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::AbilityCondition;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

/// Anoint with Affliction {1}{B} — Instant. Verbatim Oracle text.
const ANOINT: &str = "Exile target creature if it has mana value 3 or less.\nCorrupted — Exile that creature instead if its controller has three or more poison counters.";

/// CR 614.6 DOUBLE-EXECUTION WITNESS (board state).
///
/// Anoint with Affliction exiles the target ONLY if its mana value is 3 or less.
/// The "Corrupted —" line replaces that conditional exile with an unconditional
/// one, but ONLY while the target's controller has three or more poison counters.
///
/// Here the target has mana value 5 and its controller has ZERO poison counters,
/// so BOTH the printed condition (CR 608.2c) and the Corrupted override are false:
/// the creature must survive.
///
/// RED before the fix: the "Corrupted —" line was emitted as an INDEPENDENT second
/// top-level ability — `ChangeZone { destination: Exile, target: ParentTarget }`
/// with `condition: None`, because its condition ("its controller has three or
/// more poison counters") never lowered. The engine therefore exiled the creature
/// UNCONDITIONALLY, ignoring both the mana-value gate and the poison gate.
/// This assertion flips the moment that sibling is reintroduced.
#[test]
fn anoint_with_affliction_cross_line_override_does_not_exile_unconditionally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Anoint with Affliction", true, ANOINT)
        .id();
    // Mana value 5 — OUTSIDE the printed "mana value 3 or less" gate.
    let victim = {
        let mut b = scenario.add_creature(P1, "Serra Angel", 4, 4);
        b.with_mana_cost(ManaCost::generic(5));
        b.id()
    };
    let mut runner = scenario.build();

    // Control: the target's controller has no poison counters, so the Corrupted
    // override cannot apply either.
    assert_eq!(
        poison_counters(&runner, P1),
        0,
        "precondition: the Corrupted override must be OFF for this witness to \
         discriminate — if P1 had 3+ poison, exiling would be correct and the \
         test could not fail"
    );

    let outcome = runner.cast(spell).target_objects(&[victim]).resolve();

    outcome.assert_zone(&[victim], Zone::Battlefield);
}

fn poison_counters(runner: &GameRunner, player: PlayerId) -> u32 {
    runner.state().players[player.0 as usize].poison_counters
}

/// SHAPE (CR 614.15 + CR 614.6): the cross-line override must be BOUND to the
/// ability it replaces — one top-level ability whose `sub_ability` is the override,
/// gated by `ConditionInstead` — never a second, independent top-level ability.
///
/// This discriminates the two ways the runtime witness above could go green:
///   * BOUND (what we want): one ability, override as a ConditionInstead branch.
///   * merely NEUTERED: two abilities, the second an inert `Unimplemented`.
/// Without this, a regression that degraded the branch back to an honest-red
/// sibling would still pass the runtime assertion.
#[test]
fn anoint_with_affliction_binds_the_cross_line_override_as_a_branch() {
    let parsed = parse_oracle_text(
        ANOINT,
        "Anoint with Affliction",
        &[],
        &["Instant".to_string()],
        &[],
    );

    assert_eq!(
        parsed.abilities.len(),
        1,
        "CR 614.6: the \"Corrupted —\" override replaces the printed exile; it must \
         be bound INTO that ability, not published as a second independent one. \
         Two top-level abilities = the engine performs both. Got: {:#?}",
        parsed.abilities
    );

    let base = &parsed.abilities[0];
    let sub = base
        .sub_ability
        .as_ref()
        .expect("the override must be bound as the base ability's sub_ability");
    assert!(
        matches!(
            sub.condition,
            Some(AbilityCondition::ConditionInstead { .. })
        ),
        "CR 614.1a: the bound override must carry ConditionInstead so the runtime \
         SWAPS the base effect rather than running both. Got: {:?}",
        sub.condition
    );
}
