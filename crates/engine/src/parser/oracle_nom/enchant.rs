//! Shared `Enchant` keyword combinators.
//!
//! Both the multi-type `Enchant` Oracle-line parser
//! (`parser/oracle_keyword.rs::try_parse_multi_type_enchant`) and the MTGJSON
//! `FromStr` path (`types/keywords.rs::parse_enchant_target`) compose against
//! these combinators so the type-leg axis (CR 702.5a) and the optional
//! controller clause (CR 109.4) are defined exactly once.
//!
//! CR 303.4a + CR 702.5a: the "Enchant [object or player]" line is the single
//! authority for an Aura's legal target set.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::Parser;

use super::error::OracleResult;
use crate::types::ability::{ControllerRef, TargetFilter, TypeFilter, TypedFilter};

/// CR 702.5a: One enchantable core-type token. Driven by `value()` + `alt()`
/// so additional types slot in as one-line extensions.
pub(crate) fn parse_enchant_type_leg(input: &str) -> OracleResult<'_, TypeFilter> {
    alt((
        value(TypeFilter::Creature, tag("creature")),
        value(TypeFilter::Land, tag("land")),
        value(TypeFilter::Artifact, tag("artifact")),
        value(TypeFilter::Enchantment, tag("enchantment")),
        value(TypeFilter::Planeswalker, tag("planeswalker")),
        value(TypeFilter::Permanent, tag("permanent")),
        // CR 702.5a: Instant / Sorcery enable hand- and graveyard-zoned Auras
        // like Spellweaver Volute ("Enchant instant card in a graveyard").
        value(TypeFilter::Instant, tag("instant")),
        value(TypeFilter::Sorcery, tag("sorcery")),
    ))
    .parse(input)
}

/// Separator between enchant list legs. Covers serial-comma (", or "/", and "),
/// bare comma (", "), and bare conjunction (" or "/" and ") so "A, B, or C",
/// "A, B, C", and "A or B" all compose uniformly.
pub(crate) fn parse_enchant_list_sep(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag(", or "),
            tag(", and "),
            tag(", "),
            tag(" or "),
            tag(" and "),
        )),
    )
    .parse(input)
}

/// Parse a leg list with serial-comma or bare-conjunction separators.
/// Returns the list in source order.
pub(crate) fn parse_enchant_type_list(input: &str) -> OracleResult<'_, Vec<TypeFilter>> {
    use nom::multi::many0;
    use nom::sequence::preceded;

    let (input, first) = parse_enchant_type_leg(input)?;
    let (input, rest) =
        many0(preceded(parse_enchant_list_sep, parse_enchant_type_leg)).parse(input)?;
    let mut legs = Vec::with_capacity(rest.len() + 1);
    legs.push(first);
    legs.extend(rest);
    Ok((input, legs))
}

/// Optional trailing controller clause. Ordered longest-first so
/// "an opponent controls" isn't shadowed by "opponent controls".
pub(crate) fn parse_enchant_controller_suffix(input: &str) -> OracleResult<'_, ControllerRef> {
    alt((
        value(ControllerRef::You, tag(" you control")),
        value(ControllerRef::Opponent, tag(" an opponent controls")),
        value(ControllerRef::Opponent, tag(" opponent controls")),
    ))
    .parse(input)
}

/// CR 702.5d: "Enchant player" / "Enchant opponent" — the player-axis Aura.
/// The two legs yield the typed `TargetFilter` the rest of the cast pipeline
/// expects. "Enchant player" → `TargetFilter::Player` (any player at the
/// table); "Enchant opponent" → typed filter scoped to opposing players.
pub(crate) fn parse_enchant_player_base(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        value(TargetFilter::Player, tag("player")),
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag("opponent"),
        ),
    ))
    .parse(input)
}
