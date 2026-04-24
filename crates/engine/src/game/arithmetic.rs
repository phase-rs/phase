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
}
