//! Duration combinators for Oracle text parsing.
//!
//! Parses duration phrases: "until end of turn", "until the end of your/their
//! next turn", "until your/their next turn", "until end of combat",
//! "for as long as [condition]", "this turn".

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::{opt, value};
use nom::sequence::preceded;
use nom::Parser;

use super::condition::parse_inner_condition;
use super::error::OracleResult;
use crate::types::ability::{Duration, PlayerScope};

/// Parse a duration phrase from Oracle text.
///
/// Matches "until end of turn", "until the end of your/their next turn",
/// "until your/their next turn", "until end of combat", "for as long as
/// [condition]", "this turn".
pub fn parse_duration(input: &str) -> OracleResult<'_, Duration> {
    alt((
        value(Duration::UntilEndOfTurn, tag("until end of turn")),
        value(Duration::UntilEndOfCombat, tag("until end of combat")),
        parse_until_end_of_next_turn,
        parse_until_next_turn,
        value(Duration::UntilEndOfTurn, tag("this turn")),
        parse_for_as_long_as,
    ))
    .parse(input)
}

fn parse_next_turn_pronoun(input: &str) -> OracleResult<'_, PlayerScope> {
    // CR 109.5 + CR 608.2c: in this shared duration parser, "your" and
    // third-person "their" are both resolved by the caller's controller/grantee
    // binding; runtime pruning currently arms Controller-scoped durations.
    alt((
        value(PlayerScope::Controller, tag("your")),
        value(PlayerScope::Controller, tag("their")),
    ))
    .parse(input)
}

fn parse_until_end_of_next_turn(input: &str) -> OracleResult<'_, Duration> {
    // CR 514.2: "until the end of [your/their] next turn" persists through the
    // whole next turn (cleanup), distinct from "until [your/their] next turn"
    // (beginning of next turn). Match before the shorter phrase.
    let (rest, _) = tag("until the end of ").parse(input)?;
    let (rest, player) = parse_next_turn_pronoun(rest)?;
    let (rest, _) = tag(" next turn").parse(rest)?;
    Ok((rest, Duration::UntilEndOfNextTurnOf { player }))
}

fn parse_until_next_turn(input: &str) -> OracleResult<'_, Duration> {
    let (rest, _) = tag("until ").parse(input)?;
    let (rest, player) = parse_next_turn_pronoun(rest)?;
    let (rest, _) = tag(" next turn").parse(rest)?;
    Ok((rest, Duration::UntilNextTurnOf { player }))
}

/// Parse "for as long as [condition]" into `Duration::ForAsLongAs`.
///
/// CR 611.2b: "for as long as" durations embed a StaticCondition that is
/// continuously checked — effect expires when condition becomes false.
fn parse_for_as_long_as(input: &str) -> OracleResult<'_, Duration> {
    let (rest, _) = tag("for as long as ").parse(input)?;
    let (rest, condition) = parse_inner_condition(rest)?;
    Ok((rest, Duration::ForAsLongAs { condition }))
}

/// Parse an optional trailing duration: returns `Some(Duration)` if present,
/// `None` if no duration phrase follows. Does NOT consume leading whitespace.
pub fn parse_optional_duration(input: &str) -> OracleResult<'_, Option<Duration>> {
    match parse_duration(input) {
        Ok((rest, d)) => Ok((rest, Some(d))),
        Err(_) => Ok((input, None)),
    }
}

/// CR 608.2h + CR 608.2i: the cast/activation-time value-snapshot suffix.
/// CR 608.2h fixes a computed value once when the effect is applied; CR 608.2i
/// is the past-tense ("you controlled") look-back exception sharing this
/// grammar. The suffix is a pure timing marker — it does not change the object
/// filter — so callers strip it before the empty-remainder filter check and let
/// the resolver perform the snapshot.
pub fn parse_cast_snapshot_suffix(input: &str) -> OracleResult<'_, ()> {
    preceded(
        opt(tag(" ")),
        value(
            (),
            alt((
                tag("as you cast this spell"),
                tag("as you cast it"),
                tag("as you activate this ability"),
            )),
        ),
    )
    .parse(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::StaticCondition;

    #[test]
    fn test_parse_duration_end_of_turn() {
        let (rest, d) = parse_duration("until end of turn.").unwrap();
        assert_eq!(d, Duration::UntilEndOfTurn);
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_duration_end_of_combat() {
        let (rest, d) = parse_duration("until end of combat").unwrap();
        assert_eq!(d, Duration::UntilEndOfCombat);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_duration_next_turn() {
        let (rest, d) = parse_duration("until your next turn and").unwrap();
        assert_eq!(
            d,
            Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            }
        );
        assert_eq!(rest, " and");
    }

    #[test]
    fn test_parse_duration_until_end_of_next_turn() {
        let (rest, d) = parse_duration("until the end of their next turn.").unwrap();
        assert_eq!(
            d,
            Duration::UntilEndOfNextTurnOf {
                player: PlayerScope::Controller,
            }
        );
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_duration_their_next_turn() {
        let (rest, d) = parse_duration("until their next turn.").unwrap();
        assert_eq!(
            d,
            Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            }
        );
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_duration_this_turn() {
        let (rest, d) = parse_duration("this turn.").unwrap();
        assert_eq!(d, Duration::UntilEndOfTurn);
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_duration_for_as_long_as() {
        let (rest, d) = parse_duration("for as long as ~ is tapped").unwrap();
        assert_eq!(rest, "");
        match d {
            Duration::ForAsLongAs { condition } => {
                assert!(matches!(condition, StaticCondition::SourceIsTapped));
            }
            _ => panic!("expected ForAsLongAs"),
        }
    }

    #[test]
    fn test_parse_optional_duration_present() {
        let (rest, d) = parse_optional_duration("until end of turn.").unwrap();
        assert_eq!(d, Some(Duration::UntilEndOfTurn));
        assert_eq!(rest, ".");
    }

    #[test]
    fn test_parse_optional_duration_absent() {
        let (rest, d) = parse_optional_duration("and draws a card").unwrap();
        assert_eq!(d, None);
        assert_eq!(rest, "and draws a card");
    }

    #[test]
    fn test_parse_duration_failure() {
        assert!(parse_duration("permanently").is_err());
    }

    #[test]
    fn test_cast_snapshot_suffix_cast_this_spell_leading_space() {
        assert_eq!(
            parse_cast_snapshot_suffix(" as you cast this spell"),
            Ok(("", ()))
        );
    }

    #[test]
    fn test_cast_snapshot_suffix_cast_it_leading_space() {
        assert_eq!(parse_cast_snapshot_suffix(" as you cast it"), Ok(("", ())));
    }

    #[test]
    fn test_cast_snapshot_suffix_activate_ability_leading_space() {
        assert_eq!(
            parse_cast_snapshot_suffix(" as you activate this ability"),
            Ok(("", ()))
        );
    }

    #[test]
    fn test_cast_snapshot_suffix_no_leading_space() {
        assert_eq!(
            parse_cast_snapshot_suffix("as you cast this spell"),
            Ok(("", ()))
        );
    }

    #[test]
    fn test_cast_snapshot_suffix_rejects_duration() {
        assert!(parse_cast_snapshot_suffix(" until end of turn").is_err());
    }

    #[test]
    fn test_cast_snapshot_suffix_rejects_empty() {
        assert!(parse_cast_snapshot_suffix("").is_err());
    }

    #[test]
    fn test_cast_snapshot_suffix_trailing_period() {
        assert_eq!(
            parse_cast_snapshot_suffix(" as you cast this spell."),
            Ok((".", ()))
        );
    }
}
