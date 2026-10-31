//! Saturating arithmetic primitives for engine math that would otherwise be
//! vulnerable to silent integer overflow.
//!
//! The MTG Comprehensive Rules do not bound a creature's power or toughness,
//! nor do they bound the size of a zone or counter stack. In practice, realistic
//! game values never exceed a few thousand, but hostile card interactions (a
//! `+X/+X` effect where X is `ObjectCount` of a huge list, or a counter-stacking
//! loop) can push raw `i32` arithmetic past `i32::MAX` and wrap to negative.
//! A wrapped toughness silently violates CR 613 (continuous-effect application
//! order) by producing wrong P/T values that CR 704.5f state-based actions then
//! act on — killing creatures that should live.
//!
//! All P/T math in the layer system and all `usize`/`u32` → `i32` conversions
//! in dynamic quantity resolution route through this module. Saturating
//! semantics are rules-correct: clamping to `i32::MAX` / `i32::MIN` preserves
//! the direction of the effect while preventing wrap, which is the only
//! outcome consistent with the CR's intent.
//!
//! Life readings that combine `i32` life values — a Two-Headed Giant team's
//! shared total (CR 810.4), the difference of two totals, a sum of life-change
//! amounts — are computed exactly in a wider type (`i64`, or a `u32` magnitude
//! via `unsigned_abs` / `abs_diff`, which holds `i32::MIN`'s magnitude) and
//! converted back once here: [`i64_to_i32_saturating`] for a reading,
//! [`i64_to_u32_saturating`] for a payable-life budget or an unsigned delta,
//! [`u32_to_i32_saturating`] for a magnitude.
//!
//! Life totals saturate at the `i32` limits through [`saturating_life_total`],
//! the one place a total is computed; [`reported_life_magnitude`] decides what
//! a capped change reports, and [`life_changed_amount`] converts it to the
//! `LifeChanged` event field.

use std::cmp::Ordering;

/// Saturating add for power/toughness modifications in the layer system.
///
/// CR 613.3 / CR 613.4: Continuous effects in layers 7b/7c modify a
/// creature's P/T additively. This helper ensures the result of every such
/// modification is clamped into `i32` range rather than wrapping. See module
/// docs for why this is rules-correct.
#[inline]
pub fn saturating_pt_add(current: i32, delta: i32) -> i32 {
    current.saturating_add(delta)
}

/// Convert a `usize` count (hand size, graveyard size, zone length, filtered
/// object count, etc.) to `i32`, saturating at `i32::MAX` on overflow.
///
/// CR 107.1b: Numbers in Magic are integers. The engine represents them as
/// `i32`; when a dynamic quantity reference resolves from a collection's
/// `.len()`, the count must be clamped rather than wrapped so that downstream
/// arithmetic (P/T deltas, damage, counter counts) remains correct.
#[inline]
pub fn usize_to_i32_saturating(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// Convert a `u32` counter value (a single counter stack or a sum across
/// counter types) to `i32`, saturating at `i32::MAX`.
///
/// CR 122.1: Counters are nonnegative integers. `GameObject::counters` stores
/// them as `u32`; when those values feed into `i32` quantity math they must
/// be clamped.
#[inline]
pub fn u32_to_i32_saturating(n: u32) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

/// Convert an exact `i64` value — e.g. the sum of a Two-Headed Giant team's
/// life fields (CR 810.4) — to `i32`, saturating at `i32::MIN` / `i32::MAX`.
///
/// CR 107.1b: a game value such as a life total may be negative, so the bottom
/// saturates as well as the top; saturation keeps the value's sign.
#[inline]
pub fn i64_to_i32_saturating(n: i64) -> i32 {
    match i32::try_from(n) {
        Ok(value) => value,
        Err(_) if n.is_negative() => i32::MIN,
        Err(_) => i32::MAX,
    }
}

/// Convert an exact `i64` value to `u32`: a negative value reads 0 and a value
/// past `u32::MAX` saturates. Used for payable-life budgets, where a life total
/// of 0 or less affords no positive payment (CR 119.4) and 0 is always payable
/// (CR 119.4b).
#[inline]
pub fn i64_to_u32_saturating(n: i64) -> u32 {
    match u32::try_from(n) {
        Ok(value) => value,
        Err(_) if n.is_negative() => 0,
        Err(_) => u32::MAX,
    }
}

/// Convert a `u64` magnitude to `u32`, saturating at `u32::MAX`.
#[inline]
pub fn u64_to_u32_saturating(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// CR 119.3: which way a life change moves a life total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifeDirection {
    Gain,
    Loss,
}

/// The new life total after `current` changes by `delta`.
///
/// CR 119.3 + CR 107.1b: a gain or loss adjusts the life total, which may be
/// negative; the total is stored as `i32`, so the result stops at the limit
/// the change moves toward and never moves against the change. This is the one
/// place a life total is computed. `current` is the total the change adjusts:
/// the player's own life, or in Two-Headed Giant the team's exact shared total,
/// which a state this function never wrote may hold outside `i32`. To refuse at
/// a limit instead of saturating, return `None` past it (`Option<i64>`) and
/// have the Life arm, the one caller that stores a total, map `None` to
/// `ResourceOverflow`; the settle's retained refusal routing turns that into
/// the refusal, and each caller that only asks how far a change moves a total
/// must then answer for `None` too.
#[inline]
pub fn saturating_life_total(current: i64, delta: i64) -> i64 {
    let moved = current.saturating_add(delta);
    match delta.cmp(&0) {
        Ordering::Greater => current.max(moved.min(i64::from(i32::MAX))),
        Ordering::Less => current.min(moved.max(i64::from(i32::MIN))),
        Ordering::Equal => current,
    }
}

/// The magnitude a life change reports, from the rules amount and the amount
/// the total actually moved (`applied`).
///
/// CR 119.3 + CR 119.9: the rules amount; reporting the capped movement (0 at
/// the cap) would make a gain no life-gain event and stop "whenever you gain
/// life" triggers. The per-turn counters, the `LifeChanged` amount and the
/// settle's returned amount all come from here; reporting `applied` instead is
/// a one-body change.
#[inline]
pub fn reported_life_magnitude(rules: u64, applied: u64) -> u64 {
    debug_assert!(
        applied <= rules,
        "a saturating life total never moves past the rules amount"
    );
    rules
}

/// The `LifeChanged` event amount for a reported magnitude: a gain clamps at
/// `i32::MAX`, a loss at `i32::MIN`; clamping both signs to `±i32::MAX` is a
/// one-body change.
#[inline]
pub fn life_changed_amount(direction: LifeDirection, magnitude: u64) -> i32 {
    let magnitude = i64::try_from(magnitude).unwrap_or(i64::MAX);
    i64_to_i32_saturating(match direction {
        LifeDirection::Gain => magnitude,
        LifeDirection::Loss => -magnitude,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pt_add_clamps_positive_overflow() {
        assert_eq!(saturating_pt_add(i32::MAX - 1, 10), i32::MAX);
        assert_eq!(saturating_pt_add(i32::MAX, 1), i32::MAX);
    }

    #[test]
    fn pt_add_clamps_negative_overflow() {
        assert_eq!(saturating_pt_add(i32::MIN + 1, -10), i32::MIN);
        assert_eq!(saturating_pt_add(i32::MIN, -1), i32::MIN);
    }

    #[test]
    fn pt_add_preserves_normal_math() {
        assert_eq!(saturating_pt_add(3, 4), 7);
        assert_eq!(saturating_pt_add(5, -8), -3);
    }

    #[test]
    fn usize_conversion_clamps() {
        assert_eq!(usize_to_i32_saturating(0), 0);
        assert_eq!(usize_to_i32_saturating(42), 42);
        assert_eq!(usize_to_i32_saturating(usize::MAX), i32::MAX);
        assert_eq!(usize_to_i32_saturating(i32::MAX as usize + 1), i32::MAX);
    }

    #[test]
    fn u32_conversion_clamps() {
        assert_eq!(u32_to_i32_saturating(0), 0);
        assert_eq!(u32_to_i32_saturating(1_000), 1_000);
        assert_eq!(u32_to_i32_saturating(u32::MAX), i32::MAX);
    }

    #[test]
    fn i64_to_i32_conversion_saturates_at_both_limits() {
        assert_eq!(i64_to_i32_saturating(0), 0);
        assert_eq!(i64_to_i32_saturating(-5), -5);
        assert_eq!(i64_to_i32_saturating(i64::from(i32::MAX)), i32::MAX);
        assert_eq!(i64_to_i32_saturating(i64::from(i32::MIN)), i32::MIN);
        assert_eq!(i64_to_i32_saturating(i64::from(i32::MAX) + 1), i32::MAX);
        assert_eq!(i64_to_i32_saturating(i64::from(i32::MIN) - 1), i32::MIN);
        assert_eq!(i64_to_i32_saturating(i64::MAX), i32::MAX);
        assert_eq!(i64_to_i32_saturating(i64::MIN), i32::MIN);
    }

    #[test]
    fn i64_to_u32_conversion_floors_negatives_and_saturates() {
        assert_eq!(i64_to_u32_saturating(0), 0);
        assert_eq!(i64_to_u32_saturating(-1), 0);
        assert_eq!(i64_to_u32_saturating(i64::MIN), 0);
        assert_eq!(i64_to_u32_saturating(4_294_967_294), 4_294_967_294);
        assert_eq!(i64_to_u32_saturating(i64::from(u32::MAX)), u32::MAX);
        assert_eq!(i64_to_u32_saturating(i64::from(u32::MAX) + 1), u32::MAX);
    }

    #[test]
    fn saturating_life_total_stops_at_the_limit_it_moves_toward() {
        let max = i64::from(i32::MAX);
        let min = i64::from(i32::MIN);
        let u32_max = i64::from(u32::MAX);
        assert_eq!(saturating_life_total(max - 1, 2), max);
        assert_eq!(saturating_life_total(min + 1, -2), min);
        assert_eq!(saturating_life_total(max, 1), max);
        assert_eq!(saturating_life_total(min, -1), min);
        assert_eq!(saturating_life_total(5, -8), -3);
        assert_eq!(saturating_life_total(min, u32_max), max);
        assert_eq!(saturating_life_total(max, -u32_max), min);

        // A current total outside `i32` is never pulled to the limit and
        // never moved against the change.
        assert_eq!(
            saturating_life_total(4_294_967_294, 1),
            4_294_967_294,
            "a gain never lowers a total already past the top"
        );
        assert_eq!(saturating_life_total(4_294_967_294, -1), 4_294_967_293);
        assert_eq!(saturating_life_total(-4_294_967_296, -1), -4_294_967_296);
        assert_eq!(
            saturating_life_total(4_000_000_000, -3_000_000_000),
            1_000_000_000
        );
        for current in [min - 1, min, -3, 0, 7, max, max + 1] {
            assert_eq!(saturating_life_total(current, 0), current);
        }
    }

    #[test]
    fn reported_life_magnitude_is_the_rules_amount() {
        assert_eq!(reported_life_magnitude(5, 3), 5);
        assert_eq!(reported_life_magnitude(5, 5), 5);
        assert_eq!(
            reported_life_magnitude(u64::from(u32::MAX), 0),
            u64::from(u32::MAX)
        );
        assert_eq!(reported_life_magnitude(0, 0), 0);
    }

    #[test]
    fn life_changed_amount_clamps_a_gain_at_max_and_a_loss_at_min() {
        let two_pow_31 = 1_u64 << 31;
        assert_eq!(life_changed_amount(LifeDirection::Gain, 2), 2);
        assert_eq!(
            life_changed_amount(LifeDirection::Gain, u64::from(i32::MAX.unsigned_abs())),
            i32::MAX
        );
        assert_eq!(
            life_changed_amount(LifeDirection::Gain, two_pow_31),
            i32::MAX
        );
        assert_eq!(life_changed_amount(LifeDirection::Gain, u64::MAX), i32::MAX);
        assert_eq!(life_changed_amount(LifeDirection::Loss, 2), -2);
        assert_eq!(
            life_changed_amount(LifeDirection::Loss, two_pow_31 - 1),
            -i32::MAX
        );
        assert_eq!(
            life_changed_amount(LifeDirection::Loss, two_pow_31),
            i32::MIN
        );
        assert_eq!(
            life_changed_amount(LifeDirection::Loss, u64::from(u32::MAX)),
            i32::MIN
        );
        assert_eq!(life_changed_amount(LifeDirection::Loss, u64::MAX), i32::MIN);
        assert_eq!(life_changed_amount(LifeDirection::Gain, 0), 0);
        assert_eq!(life_changed_amount(LifeDirection::Loss, 0), 0);
    }

    #[test]
    fn u64_to_u32_conversion_saturates() {
        assert_eq!(u64_to_u32_saturating(0), 0);
        assert_eq!(u64_to_u32_saturating(7), 7);
        assert_eq!(u64_to_u32_saturating(u64::from(u32::MAX)), u32::MAX);
        assert_eq!(u64_to_u32_saturating(u64::from(u32::MAX) + 1), u32::MAX);
        assert_eq!(u64_to_u32_saturating(u64::MAX), u32::MAX);
    }
}
