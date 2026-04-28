//! Duration combinators for Oracle text parsing.
//!
//! Parses duration phrases: "until end of turn", "until your next turn",
//! "until end of combat", "for as long as [condition]", "this turn".

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::Parser;

use super::condition::parse_inner_condition;
use super::error::OracleResult;
use crate::types::ability::{Duration, PlayerScope};

/// Parse a duration phrase from Oracle text.
///
/// Matches "until end of turn", "until your next turn", "until end of combat",
/// "for as long as [condition]", "this turn".
pub fn parse_duration(input: &str) -> OracleResult<'_, Duration> {
    alt((
        value(Duration::UntilEndOfTurn, tag("until end of turn")),
        value(Duration::UntilEndOfCombat, tag("until end of combat")),
        value(
            Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            },
            tag("until your next turn"),
        ),
        value(Duration::UntilEndOfTurn, tag("this turn")),
        parse_for_as_long_as,
    ))
    .parse(input)
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
}
