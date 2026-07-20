//! Issue #6205 — "gain control of up to N target …" dropped its target count.
//!
//! CR 115.1d + CR 601.2c: a spell or ability that says "up to N target …" offers up to N
//! target slots. `parse_targeted_action_ast`'s `gain control of ` branch called
//! `strip_optional_target_prefix` — which already parses the quantifier — but
//! bound the returned `MultiTargetSpec` to `_`, so the count never reached
//! `ParsedEffectClause.multi_target`. Every other "up to N target" shape kept
//! its count (Call of the Death-Dweller 2, Patch Up 3, The War in Heaven 3), so
//! the defect was specific to this one verb path. It spans 7 cards, of which
//! three were genuinely under-targeted:
//!
//!   * The Super Hero Civil War — "Gain control of up to two target creatures
//!     with total mana value 6 or less" parsed max = 1 (only one selectable).
//!   * Jace, Ingenious Mind-Mage — "Gain control of up to three target
//!     creatures" parsed no count at all.
//!   * Domineering Will — "target player gains control of up to three target
//!     nonattacking creatures"; `GiveControl` shares these parse paths, so it
//!     was capped at one as well.
//!
//! The remaining four ("up to one target": Pyreswipe Hawk, Rangers of Ithilien,
//! Scroll of Isildur, Jon Irenicus) already selected correctly and only change
//! representation — from an implicit "optional (up to)" marker to an explicit
//! 0..=1 range. `up_to_one_stays_optional_with_an_explicit_upper_bound` pins
//! that their optionality (min 0) survives that move.
//!
//! The `TotalManaValue` target constraint was always parsed correctly; only the
//! count was lost, which is why the bug reads as "can only select one creature".
//!
//! Revert-proof: restoring the discarded binding drops `multi_target` back to
//! `None`/1 and the count assertions below fail.

use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, QuantityExpr};

/// Parse `oracle` and return the `(min, max)` of the `multi_target` spec on the
/// first ability or trigger whose effect transfers control — `GainControl` (the
/// controller takes it) or `GiveControl` ("target player gains control of …").
/// Both lower through the same `gain control of ` parse paths, so both are part
/// of this class.
fn control_target_range(
    oracle: &str,
    name: &str,
    types: &[&str],
    subtypes: &[&str],
) -> Option<(u32, Option<u32>)> {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &subtypes);

    let is_control =
        |e: &Effect| matches!(e, Effect::GainControl { .. } | Effect::GiveControl { .. });
    let fixed = |q: &QuantityExpr| match q {
        QuantityExpr::Fixed { value } => Some(*value as u32),
        _ => None,
    };
    let range = |spec: &Option<engine::types::ability::MultiTargetSpec>| {
        spec.as_ref().map(|s| {
            (
                fixed(&s.min).unwrap_or(u32::MAX),
                s.max.as_ref().and_then(fixed),
            )
        })
    };

    for ability in &parsed.abilities {
        if is_control(&ability.effect) {
            return range(&ability.multi_target);
        }
    }
    for trigger in &parsed.triggers {
        if let Some(exec) = trigger.execute.as_ref() {
            if is_control(&exec.effect) {
                return range(&exec.multi_target);
            }
        }
    }
    None
}

/// Just the max, for the cases where optionality is not the point.
fn gain_control_max(oracle: &str, name: &str, types: &[&str], subtypes: &[&str]) -> Option<u32> {
    control_target_range(oracle, name, types, subtypes).and_then(|(_, max)| max)
}

#[test]
fn saga_chapter_gain_control_keeps_up_to_two() {
    // The Super Hero Civil War, chapter I. The trailing duration clause and the
    // `total mana value` constraint both survive today; only the count was lost.
    let max = gain_control_max(
        "I — Gain control of up to two target creatures with total mana value 6 or less \
         for as long as this Saga remains on the battlefield.",
        "The Super Hero Civil War",
        &["Enchantment"],
        &["Saga"],
    );
    assert_eq!(
        max,
        Some(2),
        "\"up to two target creatures\" must offer 2 slots, got {max:?}"
    );
}

#[test]
fn loyalty_gain_control_keeps_up_to_three() {
    // Jace, Ingenious Mind-Mage — the same verb path, a different count, and no
    // constraint clause: pins that the fix is the verb path, not the constraint.
    let max = gain_control_max(
        "Gain control of up to three target creatures until end of turn. \
         Untap those creatures. They gain haste until end of turn.",
        "Jace, Ingenious Mind-Mage",
        &["Planeswalker"],
        &["Jace"],
    );
    assert_eq!(
        max,
        Some(3),
        "\"up to three target creatures\" must offer 3 slots, got {max:?}"
    );
}

#[test]
fn single_target_gain_control_declares_no_count() {
    // Discriminating guard: the common single-target form must NOT acquire a
    // count, so the fix cannot be "always attach a multi_target".
    let max = gain_control_max(
        "Gain control of target creature until end of turn.",
        "Act of Treason",
        &["Sorcery"],
        &[],
    );
    assert_eq!(
        max, None,
        "single-target gain control must not declare a count, got {max:?}"
    );
}

#[test]
fn up_to_one_stays_optional_with_an_explicit_upper_bound() {
    // The largest affected subgroup (Pyreswipe Hawk, Rangers of Ithilien, Scroll
    // of Isildur). These already targeted correctly; the fix moves them from an
    // implicit "optional (up to)" marker to an EXPLICIT 0..=1 range, so the risk
    // here is losing optionality, not losing the count. CR 601.2c: "up to one"
    // permits choosing zero targets, so min must stay 0.
    let range = control_target_range(
        "Whenever you expend 6, gain control of up to one target artifact \
         for as long as you control this creature.",
        "Pyreswipe Hawk",
        &["Creature"],
        &["Bird"],
    );
    assert_eq!(
        range,
        Some((0, Some(1))),
        "\"up to one target\" must stay optional (min 0) with max 1, got {range:?}"
    );
}

#[test]
fn give_control_keeps_up_to_three() {
    // Domineering Will — "target player gains control of up to three target
    // nonattacking creatures". The same `gain control of ` parse paths serve
    // `GiveControl`, so this card was capped at one target too; it is a third
    // genuinely-broken member of the class, not just a representation change.
    let max = gain_control_max(
        "Target player gains control of up to three target nonattacking creatures \
         until end of turn. Untap those creatures. They block this turn if able.",
        "Domineering Will",
        &["Instant"],
        &[],
    );
    assert_eq!(
        max,
        Some(3),
        "\"up to three target\" give-control must offer 3 slots, got {max:?}"
    );
}
