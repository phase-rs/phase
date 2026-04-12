//! Filter combinators for Oracle text parsing.
//!
//! Parses zone filters ("on the battlefield", "in your graveyard"),
//! property filters ("tapped", "untapped", "attacking", "blocking"),
//! and "with" property clauses ("with flying", "with power 3 or greater").

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::space1;
use nom::combinator::{map, value};
use nom::sequence::preceded;
use nom::Parser;

use super::error::OracleResult;
use super::primitives::{parse_article, parse_pt_modifier};
use super::quantity::parse_quantity_expr_number;
use crate::types::ability::{ControllerRef, FilterProp, QuantityExpr};
use crate::types::counter::CounterType;
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

/// Parse a zone filter phrase from Oracle text.
///
/// Matches "on the battlefield", "in your graveyard", "in your hand",
/// "in exile", "in your library", and opponent-scoped variants.
pub fn parse_zone_filter(input: &str) -> OracleResult<'_, Zone> {
    alt((
        value(Zone::Battlefield, tag("on the battlefield")),
        value(Zone::Graveyard, tag("in your graveyard")),
        value(Zone::Graveyard, tag("in a graveyard")),
        value(Zone::Graveyard, tag("in their graveyard")),
        value(Zone::Hand, tag("in your hand")),
        value(Zone::Hand, tag("in a player's hand")),
        value(Zone::Hand, tag("from your hand")),
        value(Zone::Exile, tag("in exile")),
        value(Zone::Exile, tag("from exile")),
        value(Zone::Library, tag("in your library")),
        value(Zone::Library, tag("from your library")),
        value(Zone::Stack, tag("on the stack")),
        value(Zone::Graveyard, tag("from your graveyard")),
        value(Zone::Graveyard, tag("from a graveyard")),
        value(Zone::Library, tag("of your library")),
    ))
    .parse(input)
}

/// Parse a zone owner/controller qualifier following a zone filter.
///
/// Matches "you control", "an opponent controls", "you own", "you don't control".
pub fn parse_zone_controller(input: &str) -> OracleResult<'_, ControllerRef> {
    alt((
        value(ControllerRef::You, tag("you control")),
        value(ControllerRef::Opponent, tag("an opponent controls")),
        value(ControllerRef::Opponent, tag("your opponents control")),
        value(ControllerRef::Opponent, tag("you don't control")),
    ))
    .parse(input)
}

/// Parse a property filter from Oracle text.
///
/// Matches object property keywords: "tapped", "untapped", "attacking",
/// "blocking", "token", "face down", "nontoken", "enchanted", "equipped".
pub fn parse_property_filter(input: &str) -> OracleResult<'_, FilterProp> {
    alt((
        value(FilterProp::Tapped, tag("tapped")),
        value(FilterProp::Untapped, tag("untapped")),
        value(FilterProp::Attacking, tag("attacking")),
        value(FilterProp::Blocking, tag("blocking")),
        value(FilterProp::Token, tag("token")),
        value(FilterProp::FaceDown, tag("face down")),
        value(FilterProp::Unblocked, tag("unblocked")),
        value(FilterProp::Suspected, tag("suspected")),
        value(FilterProp::EnchantedBy, tag("enchanted")),
        value(FilterProp::EquippedBy, tag("equipped")),
        value(FilterProp::Multicolored, tag("multicolored")),
        value(
            FilterProp::EnteredThisTurn,
            tag("entered the battlefield this turn"),
        ),
    ))
    .parse(input)
}

/// Parse a "with [property]" clause from Oracle text.
///
/// Matches "with flying", "with power 3 or greater", "with a +1/+1 counter",
/// "with defender", etc. Returns the FilterProp extracted from the clause.
pub fn parse_with_property(input: &str) -> OracleResult<'_, FilterProp> {
    preceded((tag("with"), space1), parse_with_inner).parse(input)
}

/// Parse the inner content of a "with" clause.
fn parse_with_inner(input: &str) -> OracleResult<'_, FilterProp> {
    alt((
        parse_with_power_constraint,
        // "with greater power" — relative to source (e.g., "can't be blocked by creatures with greater power")
        value(FilterProp::PowerGTSource, tag("greater power")),
        parse_with_toughness_constraint,
        parse_with_counter_property,
    ))
    .parse(input)
}

/// Parse "power N or greater" / "power X or less" from a "with" clause.
/// CR 208.1 + CR 107.3a: Accepts both literal integers and the variable X,
/// emitting a `QuantityExpr` so dynamic thresholds resolve at effect time.
fn parse_with_power_constraint(input: &str) -> OracleResult<'_, FilterProp> {
    let (rest, _) = tag("power ").parse(input)?;
    let (rest, value) = parse_quantity_expr_number(rest)?;
    let (rest, _) = tag(" or ").parse(rest)?;
    alt((
        map(tag("greater"), {
            let value = value.clone();
            move |_| FilterProp::PowerGE {
                value: value.clone(),
            }
        }),
        map(tag("less"), move |_| FilterProp::PowerLE {
            value: value.clone(),
        }),
    ))
    .parse(rest)
}

/// Parse "toughness greater than its power" from a "with" clause.
fn parse_with_toughness_constraint(input: &str) -> OracleResult<'_, FilterProp> {
    value(
        FilterProp::ToughnessGTPower,
        tag("toughness greater than its power"),
    )
    .parse(input)
}

/// Parse "a +1/+1 counter" / "a -1/-1 counter" from a "with" clause.
fn parse_with_counter_property(input: &str) -> OracleResult<'_, FilterProp> {
    let (rest, _) = parse_article(input)?;
    let (rest, (p, t)) = parse_pt_modifier(rest)?;
    let (rest, _) = tag(" counter").parse(rest)?;
    // Consume optional "s" for plural
    let rest = rest.strip_prefix('s').unwrap_or(rest);
    let counter_type = if p == 1 && t == 1 {
        CounterType::Plus1Plus1
    } else if p == -1 && t == -1 {
        CounterType::Minus1Minus1
    } else {
        CounterType::Generic(format!("{:+}/{:+}", p, t))
    };
    Ok((
        rest,
        FilterProp::CountersGE {
            counter_type,
            count: QuantityExpr::Fixed { value: 1 },
        },
    ))
}

/// Parse a color-as-property from Oracle text: "white", "blue", "black", "red", "green",
/// "colorless", "monocolored", "multicolored".
/// Returns a `FilterProp` for the color match.
pub fn parse_color_property(input: &str) -> OracleResult<'_, FilterProp> {
    alt((
        map(tag("white"), |_| FilterProp::HasColor {
            color: ManaColor::White,
        }),
        map(tag("blue"), |_| FilterProp::HasColor {
            color: ManaColor::Blue,
        }),
        map(tag("black"), |_| FilterProp::HasColor {
            color: ManaColor::Black,
        }),
        map(tag("red"), |_| FilterProp::HasColor {
            color: ManaColor::Red,
        }),
        map(tag("green"), |_| FilterProp::HasColor {
            color: ManaColor::Green,
        }),
        value(FilterProp::Multicolored, tag("multicolored")),
    ))
    .parse(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_zone_filter_battlefield() {
        let (rest, z) = parse_zone_filter("on the battlefield this turn").unwrap();
        assert_eq!(z, Zone::Battlefield);
        assert_eq!(rest, " this turn");
    }

    #[test]
    fn test_parse_zone_filter_graveyard() {
        let (rest, z) = parse_zone_filter("in your graveyard").unwrap();
        assert_eq!(z, Zone::Graveyard);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_zone_filter_exile() {
        let (rest, z) = parse_zone_filter("in exile").unwrap();
        assert_eq!(z, Zone::Exile);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_zone_filter_from_variants() {
        let (rest, z) = parse_zone_filter("from your hand and").unwrap();
        assert_eq!(z, Zone::Hand);
        assert_eq!(rest, " and");

        let (rest2, z2) = parse_zone_filter("from exile").unwrap();
        assert_eq!(z2, Zone::Exile);
        assert_eq!(rest2, "");

        let (rest3, z3) = parse_zone_filter("from your graveyard").unwrap();
        assert_eq!(z3, Zone::Graveyard);
        assert_eq!(rest3, "");
    }

    #[test]
    fn test_parse_zone_filter_failure() {
        assert!(parse_zone_filter("under the rug").is_err());
    }

    #[test]
    fn test_parse_property_filter_tapped() {
        let (rest, p) = parse_property_filter("tapped creatures").unwrap();
        assert_eq!(p, FilterProp::Tapped);
        assert_eq!(rest, " creatures");
    }

    #[test]
    fn test_parse_property_filter_attacking() {
        let (rest, p) = parse_property_filter("attacking").unwrap();
        assert_eq!(p, FilterProp::Attacking);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_property_filter_face_down() {
        let (rest, p) = parse_property_filter("face down").unwrap();
        assert_eq!(p, FilterProp::FaceDown);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_property_filter_suspected() {
        let (rest, p) = parse_property_filter("suspected creature").unwrap();
        assert_eq!(p, FilterProp::Suspected);
        assert_eq!(rest, " creature");
    }

    #[test]
    fn test_parse_property_filter_failure() {
        assert!(parse_property_filter("flying").is_err());
    }

    #[test]
    fn test_parse_with_power() {
        let (rest, p) = parse_with_property("with power 3 or greater").unwrap();
        assert_eq!(
            p,
            FilterProp::PowerGE {
                value: QuantityExpr::Fixed { value: 3 }
            }
        );
        assert_eq!(rest, "");

        let (rest2, p2) = parse_with_property("with power 2 or less and").unwrap();
        assert_eq!(
            p2,
            FilterProp::PowerLE {
                value: QuantityExpr::Fixed { value: 2 }
            }
        );
        assert_eq!(rest2, " and");
    }

    #[test]
    fn test_parse_with_power_x_or_greater() {
        // CR 107.3a + CR 601.2b: `with power X or greater` emits `QuantityRef::Variable`
        // — resolves against `chosen_x` at effect time via `FilterContext::from_ability`.
        use crate::types::ability::QuantityRef;
        let (rest, p) = parse_with_property("with power x or greater").unwrap();
        assert_eq!(
            p,
            FilterProp::PowerGE {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string()
                    }
                }
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_with_counter() {
        let (rest, p) = parse_with_property("with a +1/+1 counter on it").unwrap();
        assert_eq!(rest, " on it");
        match p {
            FilterProp::CountersGE {
                counter_type,
                count,
            } => {
                assert_eq!(counter_type, CounterType::Plus1Plus1);
                assert_eq!(count, QuantityExpr::Fixed { value: 1 });
            }
            _ => panic!("expected CountersGE"),
        }
    }

    #[test]
    fn test_parse_zone_controller() {
        let (rest, c) = parse_zone_controller("you control forever").unwrap();
        assert_eq!(c, ControllerRef::You);
        assert_eq!(rest, " forever");

        let (rest2, c2) = parse_zone_controller("you don't control").unwrap();
        assert_eq!(c2, ControllerRef::Opponent);
        assert_eq!(rest2, "");
    }

    #[test]
    fn test_parse_color_property() {
        let (rest, p) = parse_color_property("white creature").unwrap();
        assert_eq!(
            p,
            FilterProp::HasColor {
                color: ManaColor::White
            }
        );
        assert_eq!(rest, " creature");

        let (rest2, p2) = parse_color_property("multicolored").unwrap();
        assert_eq!(p2, FilterProp::Multicolored);
        assert_eq!(rest2, "");
    }
}
