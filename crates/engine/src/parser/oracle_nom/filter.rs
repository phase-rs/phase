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

/// Parse a *bare* zone name with NO preposition lead-in: "exile",
/// "a graveyard", "their graveyard", "a library", "their library", "the stack".
///
/// Companion to [`parse_zone_filter`] (which requires an "in/on/of/from <zone>"
/// preposition) and [`parse_enters_origin_zone`] (which requires the "from
/// <zone>" suffix). Use this ONLY where the preposition lead-in is supplied
/// separately by the caller AND that lead-in is not a bare "from " — e.g.
/// "or after being cast from <zone>", where `parse_enters_origin_zone`'s bundled
/// `tag("from exile")` does not fit because the grammatical lead-in is "being
/// cast from ". For the plain "would enter from <zone>" suffix, prefer
/// [`parse_enters_origin_zone`] directly. Composed in the same
/// `value(Zone::X, tag(...))` idiom as [`parse_zone_filter`].
pub fn parse_zone_word(input: &str) -> OracleResult<'_, Zone> {
    alt((
        value(Zone::Exile, tag("exile")),
        value(Zone::Graveyard, tag("a graveyard")),
        value(Zone::Graveyard, tag("their graveyard")),
        value(Zone::Library, tag("a library")),
        value(Zone::Library, tag("their library")),
        value(Zone::Stack, tag("the stack")),
    ))
    .parse(input)
}

/// Parse a zone owner/controller qualifier following a zone filter.
///
/// Matches "you control", "an opponent controls", "your opponents control",
/// "you don't control", "target player controls", "defending player controls".
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
        // CR 109.4 + CR 102.2 / CR 102.3: "target opponent controls" — filter
        // controller is the opponent chosen as a target. Consumer surfaces an
        // opponent-only companion slot (see `companion_target_player_legal_targets`
        // in `game/ability_utils.rs`). Runtime read identical to TargetPlayer.
        value(
            ControllerRef::TargetOpponent,
            tag("target opponent controls"),
        ),
        // CR 508.5 / CR 508.5a: "defending player controls" — the controller
        // scope is the defending player (or that player's planeswalker
        // controller / battle protector) the attacking creature is attacking.
        // Resolved per attacker at runtime by
        // `combat::defending_player_for_attacker`. Shares no prefix with the
        // arms above, so dispatch order is not load-bearing.
        value(
            ControllerRef::DefendingPlayer,
            tag("defending player controls"),
        ),
        // CR 303.4b + CR 702.5a: "enchanted player controls" — the controller
        // scope is the player the source Aura is attached to. Resolved at
        // runtime by reading `source.attached_to.as_player()`. Powers the
        // Curse cycle (Trespasser's Curse, Curse of Clinging Webs, etc.).
        value(
            ControllerRef::EnchantedPlayer,
            tag("enchanted player controls"),
        ),
        // CR 102.1: "the active player controls" — the turn player. Shares no
        // prefix with the arms above, so dispatch order is not load-bearing.
        value(
            ControllerRef::ActivePlayer,
            tag("the active player controls"),
        ),
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
        // CR 702.171b: "saddled Mount/creature" selector.
        value(FilterProp::IsSaddled, tag("saddled")),
        value(FilterProp::Attacking { defender: None }, tag("attacking")),
        value(FilterProp::Blocking, tag("blocking")),
        value(FilterProp::Token, tag("token")),
        value(FilterProp::NonToken, tag("nontoken")),
        value(FilterProp::FaceDown, tag("face down")),
        // CR 701.27g: "transformed permanent"/"transformed creature" selector.
        value(FilterProp::Transformed, tag("transformed")),
        value(FilterProp::Unblocked, tag("unblocked")),
        value(FilterProp::Suspected, tag("suspected")),
        value(FilterProp::Renowned, tag("renowned")),
        // CR 701.15b/c: standalone "goaded" designation property token.
        value(FilterProp::Goaded, tag("goaded")),
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

/// CR 113.1 + CR 113.3: an object with none of the four ability categories
/// (spell, activated, triggered, static) — i.e. "no abilities". Narrow primitive
/// shared by the target-suffix scanner (oracle_target.rs) and the search-library
/// filter scanner (oracle_effect/search.rs); each call site supplies its own
/// surrounding "with " grammar, so this matches the bare predicate only.
pub fn parse_no_abilities(input: &str) -> OracleResult<'_, FilterProp> {
    value(FilterProp::HasNoAbilities, tag("no abilities")).parse(input)
}

/// Parse the inner content of a "with" clause.
fn parse_with_inner(input: &str) -> OracleResult<'_, FilterProp> {
    alt((
        // CR 208.1 self-referential comparisons (a creature's own toughness vs its
        // own power, or own power vs own base power) — must precede the general P/T
        // combinator so they win over a numeric parse. Singular and plural
        // possessives both accepted via the shared `parse_self_referential_pt`
        // helper (also reached through `parse_pt_comparison` for the `parse_target`
        // call sites that bypass `parse_with_inner`).
        parse_self_referential_pt,
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
    // CR 208.1: self-referential comparison "(power|toughness) greater than
    // <poss> (power|toughness)" — a creature's own stat versus its own other
    // stat. This MUST precede the general "<stat> greater than <quantity>" tail,
    // which would otherwise resolve the possessive "its/their power" through the
    // quantity grammar as the *source* object's power (wrong scope for a filter
    // applied per candidate). Both possessive forms ("its" singular, "their"
    // plural) and both directions collapse to the dedicated self-referential
    // props the runtime evaluates against each candidate (`ToughnessGTPower`,
    // `PowerExceedsBase`).
    if let Ok((rest, prop)) = parse_self_referential_pt(input) {
        return Ok((rest, prop));
    }
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

/// CR 208.1: Possessive pronoun introducing a creature's *own* stat in a
/// self-referential P/T comparison — "its" (singular subject) or "their" (plural
/// subject). Both refer to the candidate object itself, not the ability source.
fn parse_pt_possessive(input: &str) -> OracleResult<'_, &str> {
    alt((tag("its"), tag("their"))).parse(input)
}

/// CR 208.1: "toughness greater than <poss> power" → [`FilterProp::ToughnessGTPower`]
/// and "power greater than <poss> base power" → [`FilterProp::PowerExceedsBase`].
/// These are the self-referential P/T comparisons (a creature's own stat vs its
/// own other stat), distinct from the numeric/quantity-threshold comparisons the
/// rest of `parse_pt_comparison` handles. Accepts singular and plural possessives.
fn parse_self_referential_pt(input: &str) -> OracleResult<'_, FilterProp> {
    alt((
        value(
            FilterProp::ToughnessGTPower,
            (
                tag("toughness greater than "),
                parse_pt_possessive,
                tag(" power"),
            ),
        ),
        value(
            FilterProp::PowerExceedsBase,
            (
                tag("power greater than "),
                parse_pt_possessive,
                tag(" base power"),
            ),
        ),
    ))
    .parse(input)
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
    // CR 208.1: Power and toughness are creature characteristics, so this
    // grammar preserves their comparison threshold as a typed quantity.
    // The threshold may be a dynamic quantity ("less than the number of …") OR a
    // literal number / X ("power less than 3", Wasp, Shrinking Savior). The
    // postfix form ("3 or less") already accepts literals via
    // `parse_quantity_expr_number`; the infix form must too, so try the dynamic
    // ref first (unchanged behavior) and fall back to the literal/X parser.
    let (rest, value) = alt((
        map(parse_quantity_ref, |qty| QuantityExpr::Ref { qty }),
        parse_quantity_expr_number,
    ))
    .parse(rest)?;
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

    // CR 702.171b: "saddled Mount/creature" selector → FilterProp::IsSaddled.
    #[test]
    fn test_parse_property_filter_saddled() {
        let (rest, p) = parse_property_filter("saddled Mount you control").unwrap();
        assert_eq!(p, FilterProp::IsSaddled);
        assert_eq!(rest, " Mount you control");
    }

    #[test]
    fn test_parse_property_filter_attacking() {
        let (rest, p) = parse_property_filter("attacking").unwrap();
        assert_eq!(p, FilterProp::Attacking { defender: None });
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_property_filter_face_down() {
        let (rest, p) = parse_property_filter("face down").unwrap();
        assert_eq!(p, FilterProp::FaceDown);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_property_filter_transformed() {
        // CR 701.27g: "transformed permanent" selector (Mutagen Connoisseur).
        let (rest, p) = parse_property_filter("transformed permanent").unwrap();
        assert_eq!(p, FilterProp::Transformed);
        assert_eq!(rest, " permanent");
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
    fn test_parse_property_filter_goaded() {
        // CR 701.15b/c: standalone "goaded" designation property token (Gap A, site 14).
        let (rest, p) = parse_property_filter("goaded creature").unwrap();
        assert_eq!(p, FilterProp::Goaded);
        assert_eq!(rest, " creature");
    }

    #[test]
    fn test_parse_property_filter_failure() {
        assert!(parse_property_filter("flying").is_err());
    }

    #[test]
    fn test_parse_no_abilities() {
        // CR 113.1 + CR 113.3: bare "no abilities" predicate → HasNoAbilities,
        // fully consumed.
        let (rest, prop) = parse_no_abilities("no abilities").unwrap();
        assert_eq!(prop, FilterProp::HasNoAbilities);
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_no_abilities_residual() {
        // Only the bare predicate is consumed; trailing grammar is left for the
        // call site's scanner.
        let (rest, prop) = parse_no_abilities("no abilities and more").unwrap();
        assert_eq!(prop, FilterProp::HasNoAbilities);
        assert_eq!(rest, " and more");
    }

    #[test]
    fn test_parse_no_abilities_failure() {
        assert!(parse_no_abilities("flying").is_err());
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
    fn test_parse_pt_comparison_infix_less_than_literal() {
        // CR 208.1 + CR 107.1: "power less than 3" (infix form with a LITERAL
        // threshold) must parse, not just the postfix "3 or less" form. Wasp,
        // Shrinking Savior: "for each creature with power less than 0". Strict
        // "less than N" lowers to LE (N-1).
        let (rest, p) = parse_pt_comparison("power less than 3").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            p,
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::LE,
                value: QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Fixed { value: 3 }),
                    offset: -1,
                },
            }
        );
    }

    #[test]
    fn test_parse_pt_comparison_infix_greater_than_literal() {
        // "toughness greater than 4" → GE (4+1) with a literal threshold.
        let (rest, p) = parse_pt_comparison("toughness greater than 4").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            p,
            FilterProp::PtComparison {
                stat: PtStat::Toughness,
                scope: PtValueScope::Current,
                comparator: Comparator::GE,
                value: QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Fixed { value: 4 }),
                    offset: 1,
                },
            }
        );
    }

    /// CR 208.1 + CR 107.1: inclusive-literal boundary — "power less than or equal
    /// to 0" keeps the LITERAL threshold with NO offset (the "or equal to" clause
    /// makes it a non-strict `LE 0`), proving the `0` boundary and the optional
    /// equal clause on the newly-admitted literal axis.
    #[test]
    fn test_parse_pt_comparison_infix_less_than_or_equal_literal() {
        let (rest, p) = parse_pt_comparison("power less than or equal to 0").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            p,
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 0 },
            }
        );
    }

    /// CR 107.3a: the newly-admitted `X` threshold on the infix form — "power less
    /// than X" lowers to `LE` of `Offset(Variable("X"), -1)`, proving the literal/X
    /// parser (not only `parse_quantity_ref`) feeds the infix tail.
    #[test]
    fn test_parse_pt_comparison_infix_less_than_x() {
        let (rest, p) = parse_pt_comparison("power less than x").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            p,
            FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Current,
                comparator: Comparator::LE,
                value: QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    }),
                    offset: -1,
                },
            }
        );
    }

    /// CR 208.1 + CR 107.1 — production-path regression (Wasp, Shrinking Savior).
    /// Parsing the FULL card through `parse_oracle_text` must retain the
    /// `power < 0` filter on the draw count: "draw a card for each creature with
    /// power less than 0" lowers to a Draw whose count is an `ObjectCount` over
    /// creatures carrying a `PtComparison(Power, …)`, not a flat draw of one.
    /// Reverting the infix-literal parser fix collapses the count to `Fixed(1)`,
    /// which makes this assertion fail — proving the whole card-conversion path,
    /// not just the isolated grammar branch, depends on the change.
    #[test]
    fn wasp_shrinking_savior_draw_count_retains_power_filter() {
        use crate::types::ability::{
            AbilityDefinition, Effect, FilterProp, PtStat, QuantityRef, TargetFilter,
        };
        fn find_draw_count(def: &AbilityDefinition) -> Option<QuantityExpr> {
            if let Effect::Draw { count, .. } = &*def.effect {
                return Some(count.clone());
            }
            def.sub_ability.as_deref().and_then(find_draw_count)
        }
        let parsed = crate::parser::parse_oracle_text(
            "Whenever Wasp attacks, up to one other target creature gets -3/-0 until your next turn. Then draw a card for each creature with power less than 0 on the battlefield.",
            "Wasp, Shrinking Savior",
            &[],
            &["Creature".to_string()],
            &[],
        );
        let count = parsed
            .triggers
            .iter()
            .filter_map(|t| t.execute.as_deref())
            .find_map(find_draw_count)
            .expect("Wasp's attack trigger must contain a Draw effect");
        let QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } = &count
        else {
            panic!("draw count must be a dynamic ObjectCount, got {count:?}");
        };
        let TargetFilter::Typed(tf) = filter else {
            panic!("ObjectCount filter must be a Typed creature filter, got {filter:?}");
        };
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::PtComparison {
                    stat: PtStat::Power,
                    ..
                }
            )),
            "the draw-count filter must retain the power comparison, got {:?}",
            tf.properties
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

    // CR 508.5 / CR 508.5a: "defending player controls" scopes the filter
    // controller to the defending player for attack-trigger targets (Kogla,
    // The Tarrasque, ~42 cards). Class-level combinator behavior, not one card.
    #[test]
    fn test_parse_zone_controller_defending_player() {
        let (rest, c) = parse_zone_controller("defending player controls").unwrap();
        assert_eq!(c, ControllerRef::DefendingPlayer);
        assert_eq!(rest, "");

        // Remainder preservation: the new arm consumes only the qualifier and
        // does not over-consume trailing text.
        let (rest2, c2) = parse_zone_controller("defending player controls and ").unwrap();
        assert_eq!(c2, ControllerRef::DefendingPlayer);
        assert_eq!(rest2, " and ");
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
