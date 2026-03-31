//! Quantity expression combinators for Oracle text parsing.
//!
//! Parses quantity expressions from Oracle text: fixed numbers, dynamic references
//! like "the number of creatures you control", "its power", "your life total",
//! "equal to" phrases, and "for each" phrases.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::{map, value};
use nom::Parser;

use super::error::OracleResult;
use super::primitives::parse_number;
use super::target::parse_type_filter_word;
use crate::types::ability::{
    ControllerRef, CountScope, QuantityExpr, QuantityRef, TargetFilter, TypedFilter,
    ZoneRef,
};

/// Parse a quantity expression: either a fixed number or a dynamic reference.
pub fn parse_quantity(input: &str) -> OracleResult<'_, QuantityExpr> {
    alt((
        map(parse_quantity_ref, |qty| QuantityExpr::Ref { qty }),
        map(parse_number, |n| QuantityExpr::Fixed { value: n as i32 }),
    ))
    .parse(input)
}

/// Parse a dynamic quantity reference from Oracle text.
///
/// Matches phrases like "the number of creatures you control", "its power",
/// "your life total", "cards in your hand", etc.
pub fn parse_quantity_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        parse_the_number_of,
        parse_life_total_ref,
        parse_hand_size_ref,
        parse_graveyard_size_ref,
        parse_self_power_ref,
        parse_self_toughness_ref,
        parse_life_lost_ref,
        parse_life_gained_ref,
        parse_starting_life_ref,
        parse_event_context_refs,
    ))
    .or(alt((
        parse_target_power_ref,
        parse_target_life_ref,
        parse_basic_land_type_count,
        parse_devotion_ref,
    )))
    .parse(input)
}

/// Parse "the number of [type] you control" → ObjectCount.
fn parse_the_number_of(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("the number of ").parse(input)?;
    parse_number_of_inner(rest)
}

/// Parse the inner part after "the number of".
fn parse_number_of_inner(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        parse_number_of_controlled_type,
        parse_number_of_cards_in_zone,
        parse_number_of_opponents,
    ))
    .parse(input)
}

/// Parse "[type(s)] you control" after "the number of".
fn parse_number_of_controlled_type(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, tf) = parse_type_filter_word(input)?;
    let (rest, _) = tag(" you control").parse(rest)?;
    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![tf],
                controller: Some(ControllerRef::You),
                properties: Vec::new(),
            }),
        },
    ))
}

/// Parse "cards in your graveyard" / "creature cards in your graveyard" after "the number of".
fn parse_number_of_cards_in_zone(input: &str) -> OracleResult<'_, QuantityRef> {
    // Optional type qualifier before "cards"
    let (rest, type_opt) = match parse_type_filter_word(input) {
        Ok((r, tf)) => {
            if let Ok((r2, _)) =
                tag::<_, _, nom_language::error::VerboseError<&str>>(" cards").parse(r)
            {
                (r2, vec![tf])
            } else {
                // No " cards" after type — not this pattern
                return parse_cards_in_zone_direct(input);
            }
        }
        Err(_) => {
            let (r, _) = tag("cards").parse(input)?;
            (r, Vec::new())
        }
    };

    // Zone reference
    let (rest, _) = tag(" in your ").parse(rest)?;
    let (rest, zone) = alt((
        value(ZoneRef::Graveyard, tag("graveyard")),
        value(ZoneRef::Library, tag("library")),
    ))
    .parse(rest)?;

    Ok((
        rest,
        QuantityRef::ZoneCardCount {
            zone,
            card_types: type_opt,
            scope: CountScope::Controller,
        },
    ))
}

/// Direct "cards in your [zone]" parse when no type prefix matched.
fn parse_cards_in_zone_direct(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("cards in your ").parse(input)?;
    let (rest, zone) = alt((
        value(ZoneRef::Graveyard, tag("graveyard")),
        value(ZoneRef::Library, tag("library")),
    ))
    .parse(rest)?;
    Ok((
        rest,
        QuantityRef::ZoneCardCount {
            zone,
            card_types: Vec::new(),
            scope: CountScope::Controller,
        },
    ))
}

/// Parse "opponents" / "opponents you have" after "the number of".
fn parse_number_of_opponents(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("opponents").parse(input)?;
    Ok((
        rest,
        QuantityRef::PlayerCount {
            filter: crate::types::ability::PlayerFilter::Opponent,
        },
    ))
}

/// Parse "your life total".
fn parse_life_total_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    value(QuantityRef::LifeTotal, tag("your life total")).parse(input)
}

/// Parse "cards in your hand".
fn parse_hand_size_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    value(QuantityRef::HandSize, tag("cards in your hand")).parse(input)
}

/// Parse "cards in your graveyard".
fn parse_graveyard_size_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    value(QuantityRef::GraveyardSize, tag("cards in your graveyard")).parse(input)
}

/// Parse "its power" / "~'s power" / "this creature's power".
fn parse_self_power_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(QuantityRef::SelfPower, tag("its power")),
        value(QuantityRef::SelfPower, tag("~'s power")),
        value(QuantityRef::SelfPower, tag("this creature's power")),
    ))
    .parse(input)
}

/// Parse "its toughness" / "~'s toughness" / "this creature's toughness".
fn parse_self_toughness_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(QuantityRef::SelfToughness, tag("its toughness")),
        value(QuantityRef::SelfToughness, tag("~'s toughness")),
        value(QuantityRef::SelfToughness, tag("this creature's toughness")),
    ))
    .parse(input)
}

/// Parse life-lost references: "the life you've lost this turn", etc.
fn parse_life_lost_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(
            QuantityRef::LifeLostThisTurn,
            tag("the life you've lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn,
            tag("the life you lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn,
            tag("life you've lost this turn"),
        ),
        value(
            QuantityRef::LifeLostThisTurn,
            tag("life you lost this turn"),
        ),
    ))
    .parse(input)
}

/// Parse life-gained references: "the life you've gained this turn", etc.
fn parse_life_gained_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(
            QuantityRef::LifeGainedThisTurn,
            tag("the life you've gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn,
            tag("the life you gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn,
            tag("life you've gained this turn"),
        ),
        value(
            QuantityRef::LifeGainedThisTurn,
            tag("life you gained this turn"),
        ),
    ))
    .parse(input)
}

/// Parse "your starting life total".
fn parse_starting_life_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    value(
        QuantityRef::StartingLifeTotal,
        tag("your starting life total"),
    )
    .parse(input)
}

/// Parse event-context quantity references.
fn parse_event_context_refs(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(QuantityRef::EventContextAmount, tag("that much")),
        value(QuantityRef::EventContextAmount, tag("that many")),
        value(
            QuantityRef::EventContextSourcePower,
            tag("that creature's power"),
        ),
        value(
            QuantityRef::EventContextSourceToughness,
            tag("that creature's toughness"),
        ),
    ))
    .parse(input)
}

/// Parse "target creature's power" / "that player's life total".
fn parse_target_power_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(QuantityRef::TargetPower, tag("target creature's power")),
        value(QuantityRef::TargetPower, tag("the target creature's power")),
    ))
    .parse(input)
}

/// Parse "target player's life total" / "that player's life total".
fn parse_target_life_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    alt((
        value(
            QuantityRef::TargetLifeTotal,
            tag("target player's life total"),
        ),
        value(
            QuantityRef::TargetLifeTotal,
            tag("that player's life total"),
        ),
    ))
    .parse(input)
}

/// Parse "the number of basic land types among lands you control" (Domain).
fn parse_basic_land_type_count(input: &str) -> OracleResult<'_, QuantityRef> {
    value(
        QuantityRef::BasicLandTypeCount,
        tag("the number of basic land types among lands you control"),
    )
    .parse(input)
}

/// Parse devotion references.
fn parse_devotion_ref(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("your devotion to ").parse(input)?;
    let (rest, color) = super::primitives::parse_color(rest)?;
    // Check for " and [color]" for multi-color devotion
    if let Ok((rest2, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>(" and ").parse(rest)
    {
        if let Ok((rest3, color2)) = super::primitives::parse_color(rest2) {
            return Ok((
                rest3,
                QuantityRef::Devotion {
                    colors: vec![color, color2],
                },
            ));
        }
    }
    Ok((
        rest,
        QuantityRef::Devotion {
            colors: vec![color],
        },
    ))
}

/// Parse "equal to [quantity]" from Oracle text.
///
/// Returns the quantity expression following "equal to ".
pub fn parse_equal_to(input: &str) -> OracleResult<'_, QuantityExpr> {
    let (rest, _) = tag("equal to ").parse(input)?;
    parse_quantity(rest)
}

/// Parse "for each [type] you control" from Oracle text.
///
/// Returns a QuantityRef::ObjectCount with the matched filter.
pub fn parse_for_each(input: &str) -> OracleResult<'_, QuantityRef> {
    let (rest, _) = tag("for each ").parse(input)?;
    parse_for_each_inner(rest)
}

/// Parse the inner content after "for each ".
fn parse_for_each_inner(input: &str) -> OracleResult<'_, QuantityRef> {
    // "[type] you control"
    let (rest, tf) = parse_type_filter_word(input)?;
    let (rest, _) = tag(" you control").parse(rest)?;
    Ok((
        rest,
        QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![tf],
                controller: Some(ControllerRef::You),
                properties: Vec::new(),
            }),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::TypeFilter;
    use crate::types::mana::ManaColor;

    #[test]
    fn test_parse_quantity_fixed() {
        let (rest, q) = parse_quantity("3 damage").unwrap();
        assert_eq!(q, QuantityExpr::Fixed { value: 3 });
        assert_eq!(rest, " damage");
    }

    #[test]
    fn test_parse_quantity_ref_life_total() {
        let (rest, q) = parse_quantity("your life total").unwrap();
        assert_eq!(
            q,
            QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_quantity_ref_hand_size() {
        let (rest, q) = parse_quantity_ref("cards in your hand").unwrap();
        assert_eq!(q, QuantityRef::HandSize);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_quantity_ref_self_power() {
        let (rest, q) = parse_quantity_ref("its power").unwrap();
        assert_eq!(q, QuantityRef::SelfPower);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_quantity_ref_graveyard() {
        let (rest, q) = parse_quantity_ref("cards in your graveyard and").unwrap();
        assert_eq!(q, QuantityRef::GraveyardSize);
        assert_eq!(rest, " and");
    }

    #[test]
    fn test_parse_quantity_ref_life_lost() {
        let (rest, q) = parse_quantity_ref("the life you've lost this turn").unwrap();
        assert_eq!(q, QuantityRef::LifeLostThisTurn);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_quantity_failure() {
        assert!(parse_quantity("xyz").is_err());
    }

    #[test]
    fn test_parse_the_number_of_creatures() {
        let (rest, q) = parse_quantity_ref("the number of creatures you control").unwrap();
        match q {
            QuantityRef::ObjectCount { filter } => match filter {
                TargetFilter::Typed(tf) => {
                    assert!(matches!(tf.type_filters[0], TypeFilter::Creature));
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                }
                _ => panic!("expected Typed filter"),
            },
            _ => panic!("expected ObjectCount"),
        }
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_event_context_refs() {
        let (rest, q) = parse_quantity_ref("that much life").unwrap();
        assert_eq!(q, QuantityRef::EventContextAmount);
        assert_eq!(rest, " life");

        let (rest2, q2) = parse_quantity_ref("that creature's power").unwrap();
        assert_eq!(q2, QuantityRef::EventContextSourcePower);
        assert_eq!(rest2, "");
    }

    #[test]
    fn test_parse_equal_to() {
        let (rest, q) = parse_equal_to("equal to its power").unwrap();
        assert_eq!(
            q,
            QuantityExpr::Ref {
                qty: QuantityRef::SelfPower
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_for_each() {
        let (rest, q) = parse_for_each("for each creature you control").unwrap();
        match q {
            QuantityRef::ObjectCount { filter } => match filter {
                TargetFilter::Typed(tf) => {
                    assert!(matches!(tf.type_filters[0], TypeFilter::Creature));
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                }
                _ => panic!("expected Typed filter"),
            },
            _ => panic!("expected ObjectCount"),
        }
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_devotion() {
        let (rest, q) = parse_quantity_ref("your devotion to red").unwrap();
        assert_eq!(
            q,
            QuantityRef::Devotion {
                colors: vec![ManaColor::Red]
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_devotion_multicolor() {
        let (rest, q) = parse_quantity_ref("your devotion to white and black").unwrap();
        assert_eq!(
            q,
            QuantityRef::Devotion {
                colors: vec![ManaColor::White, ManaColor::Black]
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_target_power() {
        let (rest, q) = parse_quantity_ref("target creature's power").unwrap();
        assert_eq!(q, QuantityRef::TargetPower);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_basic_land_type_count() {
        let (rest, q) =
            parse_quantity_ref("the number of basic land types among lands you control").unwrap();
        assert_eq!(q, QuantityRef::BasicLandTypeCount);
        assert_eq!(rest, "");
    }
}
