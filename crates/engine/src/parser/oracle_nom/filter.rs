//! Filter combinators for Oracle text parsing.
//!
//! Parses zone filters ("on the battlefield", "in your graveyard"),
//! property filters ("tapped", "untapped", "attacking", "blocking"),
//! and "with" property clauses ("with flying", "with power 3 or greater").

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::space1;
use nom::combinator::{map, opt, value};
use nom::sequence::preceded;
use nom::Parser;

use super::error::OracleResult;
use super::primitives::{parse_article, parse_pt_modifier};
use super::quantity::{parse_quantity_expr_number, parse_quantity_ref};
use crate::types::ability::{
    Comparator, ControllerRef, FilterProp, PtStat, PtValueScope, QuantityExpr,
};
#[cfg(test)]
use crate::types::counter::CounterType;
use crate::types::counter::{parse_counter_type, CounterMatch};
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

/// Parse an origin-zone qualifier for ChangesZone triggers — the "from <zone>"
/// suffix on phrases like "enters from your graveyard" / "enters from exile".
///
/// Unlike [`parse_zone_filter`], this combinator only accepts "from X" forms;
/// "in X" / "on X" / "of X" phrasings are not grammatical after a zone-change
/// verb. Keeping the axis tight prevents over-matching on unrelated text.
///
/// "Your" vs "a" graveyard both lower to `Zone::Graveyard`. Per-player origin
/// scope is not currently modeled on ChangesZone triggers.
pub fn parse_enters_origin_zone(input: &str) -> OracleResult<'_, Zone> {
    alt((
        value(Zone::Hand, tag("from your hand")),
        value(Zone::Graveyard, tag("from your graveyard")),
        value(Zone::Graveyard, tag("from a graveyard")),
        value(Zone::Exile, tag("from exile")),
        value(Zone::Library, tag("from your library")),
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
        // CR 109.4 + CR 115.1: "target player controls" — the filter controller
        // is the player chosen as a target of the enclosing ability. The
        // consumer must surface a companion TargetFilter::Player target slot
        // (see `collect_target_slots` in `game/ability_utils.rs`) so the player
        // is selected as part of target declaration.
        value(ControllerRef::TargetPlayer, tag("target player controls")),
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
        value(FilterProp::NonToken, tag("nontoken")),
        value(FilterProp::FaceDown, tag("face down")),
        value(FilterProp::Unblocked, tag("unblocked")),
        value(FilterProp::Suspected, tag("suspected")),
        value(FilterProp::Renowned, tag("renowned")),
        value(FilterProp::EnchantedBy, tag("enchanted")),
        value(FilterProp::EquippedBy, tag("equipped")),
        parse_color_property,
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
        // CR 510.1c relative comparison — must precede the general P/T
        // combinator so "toughness greater than its power" wins over a
        // "toughness <comparator>" numeric parse.
        value(
            FilterProp::ToughnessGTPower,
            tag("toughness greater than its power"),
        ),
        // CR 509.1b: "greater power" — relative to source.
        value(FilterProp::PowerGTSource, tag("greater power")),
        // CR 208: the shared power/toughness comparison combinator (handles
        // "[base ][each ](power|toughness|power or toughness) ... N or less/greater").
        parse_pt_comparison,
        parse_with_counter_property,
    ))
    .parse(input)
}

/// CR 208 + CR 208.4b + CR 613.4b: the single, shared power/toughness comparison
/// combinator. This is the canonical home for the
/// `[base ][each ](power|toughness|power or toughness|total power and toughness)
/// <comparison> N` grammar; every context (target suffixes, "with" clauses,
/// sacrifice filters) delegates here so the grammar lives in exactly one place.
///
/// Axes parsed:
/// - optional leading `each ` — the distributive qualifier in "creatures each
///   with X" (CR 109.1 / natural-language "each"). Has no semantic effect on the
///   filter ("each with X" ≡ "with X" applied per object), so it is consumed and
///   discarded.
/// - optional `base ` → `PtValueScope::Base` (CR 208.4b); otherwise `Current`.
/// - stat selector: `power or toughness` (disjunction → `AnyOf` of two
///   `PtComparison`), `total power and toughness`, `power`, or `toughness`.
/// - comparison tail: either the postfix `N or less` / `N or greater` form, or
///   the infix `less than [or equal to] N` / `greater than [or equal to] N`
///   form (resolving to LE/GE with an `Offset` for strict `<`/`>`).
pub fn parse_pt_comparison(input: &str) -> OracleResult<'_, FilterProp> {
    // Optional distributive "each " qualifier (no semantic effect).
    let (input, _) = opt(tag("each ")).parse(input)?;
    let (input, _) = opt((tag("with"), space1)).parse(input)?;
    // Optional "base " scope marker (CR 208.4b).
    let (input, scope) = map(opt(tag("base ")), |b| {
        if b.is_some() {
            PtValueScope::Base
        } else {
            PtValueScope::Current
        }
    })
    .parse(input)?;
    // Stat selector. Longer phrases must be tried before "power".
    let (input, stats): (_, &[PtStat]) = alt((
        value(
            &[PtStat::TotalPowerToughness][..],
            tag("total power and toughness"),
        ),
        value(
            &[PtStat::Power, PtStat::Toughness][..],
            tag("power or toughness"),
        ),
        value(&[PtStat::Power][..], tag("power")),
        value(&[PtStat::Toughness][..], tag("toughness")),
    ))
    .parse(input)?;
    let (rest, (comparator, value)) = parse_pt_comparison_tail(input)?;
    let props: Vec<FilterProp> = stats
        .iter()
        .map(|&stat| FilterProp::PtComparison {
            stat,
            scope,
            comparator,
            value: value.clone(),
        })
        .collect();
    let prop = if props.len() == 1 {
        props.into_iter().next().unwrap()
    } else {
        FilterProp::AnyOf { props }
    };
    Ok((rest, prop))
}

/// CR 208.1 + CR 107.3a: Parse the comparison tail of a P/T constraint, after the
/// stat word has been consumed. Returns `(Comparator, QuantityExpr)`.
///
/// Supports two grammatical forms:
/// - infix: `less than [or equal to] N` / `greater than [or equal to] N`
///   (dynamic `QuantityRef` thresholds; strict `<`/`>` lower to LE/GE with an
///   `Offset` of -1/+1).
/// - postfix: `N or less` / `N or greater` (literal or X thresholds).
fn parse_pt_comparison_tail(input: &str) -> OracleResult<'_, (Comparator, QuantityExpr)> {
    let input = input.trim_start();
    alt((
        parse_pt_infix_tail,
        parse_pt_postfix_tail,
        parse_pt_exact_tail,
    ))
    .parse(input)
}

/// Infix form: "less than [or equal to] <qty>" / "greater than [or equal to] <qty>".
fn parse_pt_infix_tail(input: &str) -> OracleResult<'_, (Comparator, QuantityExpr)> {
    let (rest, base_cmp) = alt((
        value(Comparator::LT, tag("less than")),
        value(Comparator::GT, tag("greater than")),
    ))
    .parse(input)?;
    let rest = rest.trim_start();
    let (rest, includes_equal) = map(opt(tag("or equal to")), |e| e.is_some()).parse(rest)?;
    let rest = rest.trim_start();
    let (rest, qty) = parse_quantity_ref(rest)?;
    let value = QuantityExpr::Ref { qty };
    // Strict `<`/`>` lower to LE/GE by shifting the threshold by ∓1 (CR 107.1:
    // integers only, so "less than N" ≡ "≤ N-1").
    let (comparator, value) = match (base_cmp, includes_equal) {
        (Comparator::LT, true) => (Comparator::LE, value),
        (Comparator::GT, true) => (Comparator::GE, value),
        (Comparator::LT, false) => (
            Comparator::LE,
            QuantityExpr::Offset {
                inner: Box::new(value),
                offset: -1,
            },
        ),
        (Comparator::GT, false) => (
            Comparator::GE,
            QuantityExpr::Offset {
                inner: Box::new(value),
                offset: 1,
            },
        ),
        _ => unreachable!("base_cmp is only LT or GT"),
    };
    Ok((rest, (comparator, value)))
}

/// Postfix form: "<qty> or less" / "<qty> or greater".
fn parse_pt_postfix_tail(input: &str) -> OracleResult<'_, (Comparator, QuantityExpr)> {
    let input = input.trim_start();
    let (rest, value) = parse_quantity_expr_number(input)?;
    let rest = rest.trim_start();
    alt((
        map(tag("or less"), {
            let value = value.clone();
            move |_| (Comparator::LE, value.clone())
        }),
        map(tag("or greater"), move |_| (Comparator::GE, value.clone())),
    ))
    .parse(rest)
}

/// Exact form: "<qty>".
fn parse_pt_exact_tail(input: &str) -> OracleResult<'_, (Comparator, QuantityExpr)> {
    let input = input.trim_start();
    let (rest, value) = parse_quantity_expr_number(input)?;
    Ok((rest, (Comparator::EQ, value)))
}

/// Parse "a +1/+1 counter" / "a -1/-1 counter" from a "with" clause.
fn parse_with_counter_property(input: &str) -> OracleResult<'_, FilterProp> {
    let (rest, _) = parse_article(input)?;
    let (rest, (p, t)) = parse_pt_modifier(rest)?;
    let (rest, _) = tag(" counter").parse(rest)?;
    // Consume optional "s" for plural
    let rest = rest.strip_prefix('s').unwrap_or(rest);
    let counter_type = parse_counter_type(&format!("{p:+}/{t:+}"));
    Ok((
        rest,
        FilterProp::Counters {
            counters: CounterMatch::OfType(counter_type),
            comparator: Comparator::GE,
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
        value(
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 0,
            },
            tag("colorless"),
        ),
        value(
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 1,
            },
            tag("monocolored"),
        ),
        value(
            FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 2,
            },
            tag("multicolored"),
        ),
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
    fn test_parse_property_filter_renowned() {
        let (rest, p) = parse_property_filter("renowned creature").unwrap();
        assert_eq!(p, FilterProp::Renowned);
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
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed { value: 3 }
            }
        );
        assert_eq!(rest, "");

        let (rest2, p2) = parse_with_property("with power 2 or less and").unwrap();
        assert_eq!(
            p2,
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::LE,
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
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::GE,
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
    fn test_parse_pt_comparison_base_disjunction() {
        // CR 208.4b: "base power or toughness 1 or less" → AnyOf of two
        // Base-scope PtComparison props (the Angelic Aberration sacrifice filter).
        let (rest, p) = parse_pt_comparison("base power or toughness 1 or less").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            p,
            FilterProp::AnyOf {
                props: vec![
                    FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Base,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                    FilterProp::PtComparison {
                        stat: PtStat::Toughness,
                        scope: PtValueScope::Base,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                ]
            }
        );
    }

    #[test]
    fn test_parse_pt_comparison_total_power_toughness() {
        let (rest, p) = parse_pt_comparison("total power and toughness 5 or less").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            p,
            FilterProp::PtComparison {
                stat: PtStat::TotalPowerToughness,
                scope: PtValueScope::Current,
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 5 },
            }
        );
    }

    #[test]
    fn test_parse_pt_comparison_exact_base_power() {
        let (rest, p) = parse_with_property("with base power 1").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            p,
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Base,
                comparator: Comparator::EQ,
                value: QuantityExpr::Fixed { value: 1 },
            }
        );
    }

    #[test]
    fn test_parse_pt_comparison_each_with_qualifier() {
        // The distributive "each with" qualifier is consumed; the emitted prop
        // is identical to the plain "with" form.
        let (rest, p) = parse_pt_comparison("each with base toughness 3 or greater").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            p,
            FilterProp::PtComparison {
                stat: PtStat::Toughness,
                scope: PtValueScope::Base,
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed { value: 3 },
            }
        );
    }

    #[test]
    fn test_parse_pt_comparison_plain_current() {
        // No "base" → Current scope; single-stat "power 2 or less".
        let (rest, p) = parse_pt_comparison("power 2 or less").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            p,
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 2 },
            }
        );
    }

    #[test]
    fn test_parse_with_counter() {
        let (rest, p) = parse_with_property("with a +1/+1 counter on it").unwrap();
        assert_eq!(rest, " on it");
        match p {
            FilterProp::Counters {
                counters,
                comparator,
                count,
            } => {
                assert_eq!(counters, CounterMatch::OfType(CounterType::Plus1Plus1));
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(count, QuantityExpr::Fixed { value: 1 });
            }
            _ => panic!("expected Counters"),
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
        assert_eq!(
            p2,
            FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 2,
            }
        );
        assert_eq!(rest2, "");

        let (rest3, p3) = parse_color_property("monocolored").unwrap();
        assert_eq!(
            p3,
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 1,
            }
        );
        assert_eq!(rest3, "");
    }
}
