//! Condition combinators for Oracle text parsing.
//!
//! Parses condition phrases: "if [condition]", "as long as [condition]",
//! "unless [condition]" into typed `StaticCondition` values.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::{map, value};
use nom::sequence::preceded;
use nom::Parser;

use super::error::OracleResult;
use super::primitives::parse_number;
use crate::types::ability::{Comparator, QuantityExpr, QuantityRef, StaticCondition};

/// Parse a condition phrase from Oracle text.
///
/// Matches patterns like "if you control a creature", "as long as you have no
/// cards in hand", "unless an opponent controls a creature".
pub fn parse_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        preceded(tuple_ws_tag("if "), parse_inner_condition),
        preceded(tuple_ws_tag("as long as "), parse_inner_condition),
        preceded(tuple_ws_tag("unless "), parse_unless_condition),
    ))
    .parse(input)
}

/// Parse an "if" or "as long as" condition without the prefix keyword.
///
/// Useful when the prefix has already been consumed by the caller.
pub fn parse_inner_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        parse_turn_conditions,
        parse_source_state_conditions,
        parse_hand_conditions,
        parse_control_conditions,
        parse_life_conditions,
        parse_zone_conditions,
    ))
    .parse(input)
}

/// Helper: tag with potential leading whitespace trimmed.
fn tuple_ws_tag(t: &str) -> impl FnMut(&str) -> OracleResult<'_, &str> + '_ {
    move |input: &str| tag(t).parse(input)
}

/// Parse turn-based conditions.
fn parse_turn_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        value(StaticCondition::DuringYourTurn, tag("it's your turn")),
        value(StaticCondition::DuringYourTurn, tag("it is your turn")),
        // "it's not your turn" → Not(DuringYourTurn)
        map(tag("it's not your turn"), |_| StaticCondition::Not {
            condition: Box::new(StaticCondition::DuringYourTurn),
        }),
    ))
    .parse(input)
}

/// Parse source-state conditions (tapped, untapped, in-zone).
fn parse_source_state_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        value(StaticCondition::SourceIsTapped, tag("~ is tapped")),
        // "~ is untapped" → Not(SourceIsTapped) per existing convention
        map(tag("~ is untapped"), |_| StaticCondition::Not {
            condition: Box::new(StaticCondition::SourceIsTapped),
        }),
        value(
            StaticCondition::SourceEnteredThisTurn,
            tag("~ entered the battlefield this turn"),
        ),
        value(StaticCondition::IsRingBearer, tag("~ is your ring-bearer")),
    ))
    .parse(input)
}

/// Parse hand-related conditions.
fn parse_hand_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // "you have no cards in hand"
        map(tag("you have no cards in hand"), |_| {
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize,
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            }
        }),
        // "you have seven or more cards in hand"
        parse_cards_in_hand_ge,
    ))
    .parse(input)
}

/// Parse "you have N or more cards in hand".
fn parse_cards_in_hand_ge(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you have ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    let (rest, _) = tag(" or more cards in hand").parse(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::HandSize,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// Parse "you control" condition patterns.
fn parse_control_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        // "you control a [type]" → IsPresent with filter
        map(tag("you control a creature"), |_| {
            StaticCondition::IsPresent {
                filter: Some(crate::types::ability::TargetFilter::Typed(
                    crate::types::ability::TypedFilter {
                        type_filters: vec![crate::types::ability::TypeFilter::Creature],
                        controller: Some(crate::types::ability::ControllerRef::You),
                        properties: Vec::new(),
                    },
                )),
            }
        }),
        map(tag("you control an artifact"), |_| {
            StaticCondition::IsPresent {
                filter: Some(crate::types::ability::TargetFilter::Typed(
                    crate::types::ability::TypedFilter {
                        type_filters: vec![crate::types::ability::TypeFilter::Artifact],
                        controller: Some(crate::types::ability::ControllerRef::You),
                        properties: Vec::new(),
                    },
                )),
            }
        }),
        map(tag("you control an enchantment"), |_| {
            StaticCondition::IsPresent {
                filter: Some(crate::types::ability::TargetFilter::Typed(
                    crate::types::ability::TypedFilter {
                        type_filters: vec![crate::types::ability::TypeFilter::Enchantment],
                        controller: Some(crate::types::ability::ControllerRef::You),
                        properties: Vec::new(),
                    },
                )),
            }
        }),
    ))
    .parse(input)
}

/// Parse life-related conditions.
fn parse_life_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((parse_life_le, parse_life_ge)).parse(input)
}

/// Parse "your life total is N or less".
fn parse_life_le(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("your life total is ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    let (rest, _) = tag(" or less").parse(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal,
            },
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// Parse "your life total is N or greater" / "you have N or more life".
fn parse_life_ge(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((parse_life_ge_formal, parse_life_ge_informal)).parse(input)
}

fn parse_life_ge_formal(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("your life total is ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    let (rest, _) = tag(" or greater").parse(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

fn parse_life_ge_informal(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, _) = tag("you have ").parse(input)?;
    let (rest, n) = parse_number(rest)?;
    let (rest, _) = tag(" or more life").parse(rest)?;
    Ok((
        rest,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// Parse zone-related conditions ("~ is in your graveyard").
fn parse_zone_conditions(input: &str) -> OracleResult<'_, StaticCondition> {
    alt((
        value(
            StaticCondition::SourceInZone {
                zone: crate::types::zones::Zone::Graveyard,
            },
            tag("~ is in your graveyard"),
        ),
        value(
            StaticCondition::SourceInZone {
                zone: crate::types::zones::Zone::Graveyard,
            },
            tag("this card is in your graveyard"),
        ),
        value(
            StaticCondition::SourceInZone {
                zone: crate::types::zones::Zone::Exile,
            },
            tag("~ is in exile"),
        ),
    ))
    .parse(input)
}

/// Parse an "unless" condition, wrapping the inner condition in `Not`.
fn parse_unless_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (rest, inner) = parse_inner_condition(input)?;
    Ok((
        rest,
        StaticCondition::Not {
            condition: Box::new(inner),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_condition_your_turn() {
        let (rest, c) = parse_condition("if it's your turn, do").unwrap();
        assert_eq!(rest, ", do");
        assert_eq!(c, StaticCondition::DuringYourTurn);
    }

    #[test]
    fn test_parse_condition_as_long_as_tapped() {
        let (rest, c) = parse_condition("as long as ~ is tapped").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(c, StaticCondition::SourceIsTapped));
    }

    #[test]
    fn test_parse_condition_no_cards() {
        let (rest, c) = parse_condition("if you have no cards in hand").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                comparator, rhs, ..
            } => {
                assert_eq!(comparator, Comparator::EQ);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 0 });
            }
            _ => panic!("expected QuantityComparison"),
        }
    }

    #[test]
    fn test_parse_condition_not_your_turn() {
        let (rest, c) = parse_condition("if it's not your turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::Not { condition } => {
                assert_eq!(*condition, StaticCondition::DuringYourTurn);
            }
            _ => panic!("expected Not(DuringYourTurn)"),
        }
    }

    #[test]
    fn test_parse_condition_seven_cards() {
        let (rest, c) = parse_condition("if you have seven or more cards in hand").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                comparator, rhs, ..
            } => {
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 7 });
            }
            _ => panic!("expected QuantityComparison"),
        }
    }

    #[test]
    fn test_parse_condition_life_le() {
        let (rest, c) = parse_condition("if your life total is 5 or less").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::QuantityComparison {
                comparator, rhs, ..
            } => {
                assert_eq!(comparator, Comparator::LE);
                assert_eq!(rhs, QuantityExpr::Fixed { value: 5 });
            }
            _ => panic!("expected QuantityComparison"),
        }
    }

    #[test]
    fn test_parse_condition_unless() {
        let (rest, c) = parse_condition("unless it's your turn").unwrap();
        assert_eq!(rest, "");
        match c {
            StaticCondition::Not { condition } => {
                assert_eq!(*condition, StaticCondition::DuringYourTurn);
            }
            _ => panic!("expected Not(DuringYourTurn)"),
        }
    }

    #[test]
    fn test_parse_condition_source_in_graveyard() {
        let (rest, c) = parse_condition("as long as ~ is in your graveyard").unwrap();
        assert_eq!(rest, "");
        assert!(matches!(
            c,
            StaticCondition::SourceInZone {
                zone: crate::types::zones::Zone::Graveyard
            }
        ));
    }

    #[test]
    fn test_parse_condition_ring_bearer() {
        let (rest, c) = parse_condition("as long as ~ is your ring-bearer").unwrap();
        assert_eq!(rest, "");
        assert_eq!(c, StaticCondition::IsRingBearer);
    }

    #[test]
    fn test_parse_condition_failure() {
        assert!(parse_condition("when something happens").is_err());
    }
}
