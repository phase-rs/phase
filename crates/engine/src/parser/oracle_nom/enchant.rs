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
use crate::types::ability::{
    AttachmentKind, ControllerRef, FilterProp, TargetFilter, TypeFilter, TypedFilter,
};

/// CR 702.5a: One enchantable core-type or land-subtype token. Driven by
/// `value()` + `alt()` so additional types slot in as one-line extensions.
///
/// Basic land subtypes (Forest, Plains, Island, Swamp, Mountain) are included
/// per CR 205.3i — basic land types are the canonical Aura targets for
/// "enchant Forest" / "enchant Plains" patterns used by Old-Growth Troll
/// (KHM) and Harold and Bob, First Numens (FIN-precon). The longest-first
/// ordering inside each cluster keeps "creature" from short-matching against
/// future hypothetical subtype legs.
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
        // CR 205.3i + CR 702.5a: Basic land subtypes. Used by
        // "enchant Forest you control" (Old-Growth Troll, Harold and Bob).
        value(TypeFilter::Subtype("Forest".to_string()), tag("forest")),
        value(TypeFilter::Subtype("Plains".to_string()), tag("plains")),
        value(TypeFilter::Subtype("Island".to_string()), tag("island")),
        value(TypeFilter::Subtype("Swamp".to_string()), tag("swamp")),
        value(TypeFilter::Subtype("Mountain".to_string()), tag("mountain")),
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

/// CR 303.4 + CR 702.5a + CR 301.5: Optional trailing attachment qualifier on an
/// "Enchant <type>" line — "with another Aura attached to it" (Daybreak Coronet)
/// further restricts the legal target set to objects that already carry an
/// attachment of the named kind. "Another" is material once SBA attachment
/// legality rechecks the Aura already attached to its host, so preserve it as a
/// source-exclusion axis on the `HasAttachment` filter prop. The leading space
/// ensures the qualifier only matches after a preceding type leg (never as a
/// standalone clause).
pub(crate) fn parse_enchant_attachment_qualifier(input: &str) -> OracleResult<'_, FilterProp> {
    let (input, _) = tag(" with ").parse(input)?;
    let (input, exclude_source) = alt((
        value(true, tag("another ")),
        value(false, tag("an ")),
        value(false, tag("a ")),
    ))
    .parse(input)?;
    let (input, kind) = alt((
        value(AttachmentKind::Aura, tag("aura")),
        value(AttachmentKind::Equipment, tag("equipment")),
    ))
    .parse(input)?;
    let (input, _) = tag(" attached to it").parse(input)?;
    Ok((
        input,
        FilterProp::HasAttachment {
            kind,
            controller: None,
            exclude_source,
        },
    ))
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

/// CR 702.5a + CR 109.4: Compose `parse_enchant_type_list` with the optional
/// `parse_enchant_controller_suffix` to build a complete `TargetFilter` for an
/// inline "enchant <X>" phrase such as the one inside a return-as-Aura
/// sub-effect ("It's an Aura enchantment with enchant Forest you control").
///
/// Used by `oracle_nom::return_as_aura::try_parse` to extract the enchant
/// filter from a chunked Oracle text body. The output filter is the SAME shape
/// other Aura parsers produce so the resolver and layer system treat the
/// runtime Aura identically regardless of whether it was cast normally or
/// installed by a return-as-Aura effect.
pub(crate) fn parse_enchant_target_full(input: &str) -> OracleResult<'_, TargetFilter> {
    use nom::combinator::opt;

    let (input, type_legs) = parse_enchant_type_list(input)?;
    let (input, controller) = opt(parse_enchant_controller_suffix).parse(input)?;
    let (input, attachment) = opt(parse_enchant_attachment_qualifier).parse(input)?;

    let mut typed = TypedFilter {
        type_filters: type_legs,
        ..TypedFilter::default()
    };
    if let Some(c) = controller {
        typed.controller = Some(c);
    }
    if let Some(prop) = attachment {
        typed.properties.push(prop);
    }
    Ok((input, TargetFilter::Typed(typed)))
}
