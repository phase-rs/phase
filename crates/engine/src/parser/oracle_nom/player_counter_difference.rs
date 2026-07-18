//! CR 122.1 + CR 122.1f + CR 107.1b: threshold player-counter top-up rider.
//!
//! Detects the whole-sentence "top a player up to a threshold" form:
//!
//! ```text
//! [then, ]if target player has fewer than <N> <kind> counters,
//!   they get a number of <kind> counters equal to the difference[.]
//! ```
//!
//! Vraska, Betrayal's Sting [-9] is the canonical member: "If target player has
//! fewer than nine poison counters, they get a number of poison counters equal
//! to the difference." (CR 122.1 — a counter is a marker placed on a player;
//! CR 122.1f — ten poison counters lose the game, so nine is the pre-loss
//! threshold this tops toward).
//!
//! This is a rider, dispatched by `parse_effect_chain_ir` BEFORE
//! `split_leading_conditional` severs the sentence at its comma. The whole
//! sentence must be captured together because the count is
//! `max(0, N − current_<kind>)`: splitting off the "If …" clause as a pass/fail
//! condition would drop the "equal to the difference" arithmetic. Instead the
//! effect always resolves and the `ClampMin { minimum: 0 }` the caller builds
//! from the returned threshold makes the already-at-or-above-`N` case a zero
//! no-op (CR 107.1b — a negative calculation result is treated as zero).
//!
//! Returns `Some((kind, threshold_N, target))` only when the target is a
//! player, both counter kinds match, and the full sentence is consumed.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::char;
use nom::combinator::opt;
use nom::Parser;

use super::error::OracleError;
use super::primitives::{parse_number, parse_player_counter_kind};
use crate::parser::oracle_target::parse_target;
use crate::types::ability::TargetFilter;
use crate::types::player::PlayerCounterKind;

/// Parse the threshold poison/player-counter top-up sentence.
///
/// `lower` is the pre-lowercased chunk text. The whole sentence is consumed on
/// success; any trailing content (a following independent clause) makes this
/// decline so the generic dispatch path handles it.
pub fn try_parse(lower: &str) -> Option<(PlayerCounterKind, i32, TargetFilter)> {
    // CR 608.2c: optional leading conditional connector.
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("if "), tag("then, if ")))
        .parse(lower)
        .ok()?;

    // "target player" → TargetFilter::Player (reject any non-player target).
    // `parse_target` lower-cases internally, so a lowercase remainder feeds the
    // downstream lowercase tags cleanly.
    let (target, rest) = parse_target(rest);
    if !matches!(target, TargetFilter::Player) {
        return None;
    }

    // "has fewer than <N> " — the threshold count.
    let (rest, _) = tag::<_, _, OracleError<'_>>(" has fewer than ")
        .parse(rest)
        .ok()?;
    let (rest, threshold) = parse_number(rest).ok()?;
    let (rest, _) = char::<_, OracleError<'_>>(' ').parse(rest).ok()?;

    // First counter kind (e.g. "poison"). The following " counters" tag enforces
    // the word boundary after the matched kind.
    let (rest, kind1) = parse_player_counter_kind(rest).ok()?;

    let (rest, _) = tag::<_, _, OracleError<'_>>(" counters, they get a number of ")
        .parse(rest)
        .ok()?;

    // Second counter kind MUST equal the first ("a number of <kind> counters").
    let (rest, kind2) = parse_player_counter_kind(rest).ok()?;
    if kind1 != kind2 {
        return None;
    }

    let (rest, _) = tag::<_, _, OracleError<'_>>(" counters equal to the difference")
        .parse(rest)
        .ok()?;
    let (rest, _) = opt(char::<_, OracleError<'_>>('.')).parse(rest).ok()?;

    // Full-sentence consumption: anything left means this is not the pure
    // threshold form — decline so normal dispatch handles the remainder.
    if !rest.trim().is_empty() {
        return None;
    }

    Some((kind1, threshold as i32, target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vraska_minus9_sentence() {
        let text = "if target player has fewer than nine poison counters, they get a number of poison counters equal to the difference.";
        let (kind, threshold, target) = try_parse(text).expect("must parse");
        assert_eq!(kind, PlayerCounterKind::Poison);
        assert_eq!(threshold, 9);
        assert_eq!(target, TargetFilter::Player);
    }

    #[test]
    fn parses_without_trailing_period() {
        let text = "if target player has fewer than nine poison counters, they get a number of poison counters equal to the difference";
        assert!(try_parse(text).is_some());
    }

    #[test]
    fn rejects_mismatched_kinds() {
        let text = "if target player has fewer than nine poison counters, they get a number of rad counters equal to the difference.";
        assert!(try_parse(text).is_none());
    }

    #[test]
    fn rejects_non_player_target() {
        let text = "if target creature has fewer than nine poison counters, they get a number of poison counters equal to the difference.";
        assert!(try_parse(text).is_none());
    }

    #[test]
    fn rejects_unrelated_conditional() {
        // A generic "if X, draw a card" sentence must fall through to normal dispatch.
        let text = "if you control a swamp, draw a card.";
        assert!(try_parse(text).is_none());
    }

    #[test]
    fn rejects_object_counter_kind() {
        // "+1/+1" is an object counter, not a player counter — reject.
        let text = "if target player has fewer than nine +1/+1 counters, they get a number of +1/+1 counters equal to the difference.";
        assert!(try_parse(text).is_none());
    }
}
