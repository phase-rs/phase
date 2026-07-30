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
use super::target::parse_supertype_prefix;
use crate::parser::oracle_target::parse_without_keyword_suffix;
use crate::parser::oracle_util::parse_subtype;
use crate::types::ability::{
    AttachmentKind, ControllerRef, FilterProp, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::{noncreature_subtype_set, SubtypeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnchantTypeLeg {
    pub(crate) type_filter: TypeFilter,
    pub(crate) properties: Vec<FilterProp>,
}

/// CR 702.5a: One enchantable core-type or supported subtype token. Core
/// types and established basic-land subtype legs stay as literal nom arms;
/// artifact subtypes delegate to the canonical subtype classifier below.
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
        value(TypeFilter::Artifact, tag("artifact")),
        // CR 205.3g + CR 702.5a: Artifact subtype legs use the canonical
        // subtype registry. This must precede `land` so `Lander` is not
        // short-matched as the core Land type.
        parse_artifact_subtype_enchant_leg,
        value(TypeFilter::Land, tag("land")),
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

fn parse_artifact_subtype_enchant_leg(input: &str) -> OracleResult<'_, TypeFilter> {
    let Some((subtype, consumed)) = parse_subtype(input) else {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    };

    if !matches!(
        noncreature_subtype_set(&subtype),
        Some(SubtypeSet::Artifact)
    ) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }

    Ok((&input[consumed..], TypeFilter::Subtype(subtype)))
}

/// CR 205.4a + CR 702.5a: An Enchant type leg may carry a supertype adjective
/// such as "snow land", "basic land", or "legendary creature". Reuse the
/// shared target-phrase supertype recognizer so Aura legality gets the same
/// `HasSupertype` property as ordinary target phrases.
pub(crate) fn parse_enchant_qualified_type_leg(input: &str) -> OracleResult<'_, EnchantTypeLeg> {
    use nom::combinator::opt;

    let (input, supertype) = opt(parse_supertype_prefix).parse(input)?;
    let (input, type_filter) = parse_enchant_type_leg(input)?;
    let properties = supertype
        .map(|value| FilterProp::HasSupertype { value })
        .into_iter()
        .collect();
    Ok((
        input,
        EnchantTypeLeg {
            type_filter,
            properties,
        },
    ))
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
pub(crate) fn parse_enchant_type_list(input: &str) -> OracleResult<'_, Vec<EnchantTypeLeg>> {
    use nom::multi::many0;
    use nom::sequence::preceded;

    let (input, first) = parse_enchant_qualified_type_leg(input)?;
    let (input, rest) = many0(preceded(
        parse_enchant_list_sep,
        parse_enchant_qualified_type_leg,
    ))
    .parse(input)?;
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
            exclude_source: if exclude_source {
                crate::types::ability::SourceExclusion::Exclude
            } else {
                crate::types::ability::SourceExclusion::Include
            },
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
    let (input, without_keyword) = parse_enchant_without_keyword_suffix(input)?;

    let mut filters = Vec::with_capacity(type_legs.len());
    for leg in type_legs {
        let mut typed = TypedFilter::new(leg.type_filter);
        if let Some(c) = controller.clone() {
            typed = typed.controller(c);
        }

        let mut properties = leg.properties;
        if let Some(prop) = attachment.clone() {
            properties.push(prop);
        }
        properties.extend(without_keyword.iter().cloned());
        if !properties.is_empty() {
            typed = typed.properties(properties);
        }

        filters.push(TargetFilter::Typed(typed));
    }

    let filter = if filters.len() == 1 {
        filters.pop().unwrap()
    } else {
        TargetFilter::Or { filters }
    };
    Ok((input, filter))
}

/// CR 702.5a + CR 702.9: Optional trailing "without [keyword]" qualifier on an
/// enchant line (Trapped in the Tower, Roots). Delegates to the shared target
/// suffix authority so Aura legal-target sets match `parse_type_phrase`.
fn parse_enchant_without_keyword_suffix(input: &str) -> OracleResult<'_, Vec<FilterProp>> {
    match parse_without_keyword_suffix(input) {
        Some((props, consumed)) => Ok((&input[consumed..], props)),
        None => Ok((input, Vec::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::keywords::Keyword;

    /// CR 702.5a + CR 702.9: Trapped in the Tower — "Enchant creature without flying".
    #[test]
    fn parse_enchant_target_creature_without_flying() {
        let (rest, filter) =
            parse_enchant_target_full("creature without flying").expect("must parse");
        assert!(rest.is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(
            tf.properties.iter().any(
                |p| matches!(p, FilterProp::WithoutKeyword { value } if *value == Keyword::Flying)
            ),
            "expected WithoutKeyword(Flying), got {:?}",
            tf.properties
        );
    }

    /// CR 205.4a + CR 702.5a: On Thin Ice-style supertype-qualified Aura
    /// targets must preserve both the head type and the supertype restriction.
    #[test]
    fn parse_enchant_target_snow_land_you_control() {
        use crate::types::card_type::Supertype;

        let (rest, filter) =
            parse_enchant_target_full("snow land you control").expect("must parse");
        assert!(rest.is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected Typed filter, got {filter:?}");
        };
        assert_eq!(tf.type_filters, vec![TypeFilter::Land]);
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(tf.properties.contains(&FilterProp::HasSupertype {
            value: Supertype::Snow
        }));
    }

    /// CR 205.4a + CR 702.5a: Multi-leg inline Enchant phrases must keep a
    /// qualified leg's supertype property scoped to that leg.
    #[test]
    fn parse_enchant_target_multi_leg_keeps_supertype_per_leg() {
        use crate::types::card_type::Supertype;

        let (rest, filter) =
            parse_enchant_target_full("legendary creature or planeswalker").expect("must parse");
        assert!(rest.is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = filter else {
            panic!("expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);

        let TargetFilter::Typed(first) = &filters[0] else {
            panic!("expected first Typed leg");
        };
        assert_eq!(first.type_filters, vec![TypeFilter::Creature]);
        assert!(first.properties.contains(&FilterProp::HasSupertype {
            value: Supertype::Legendary
        }));

        let TargetFilter::Typed(second) = &filters[1] else {
            panic!("expected second Typed leg");
        };
        assert_eq!(second.type_filters, vec![TypeFilter::Planeswalker]);
        assert!(
            !second
                .properties
                .iter()
                .any(|prop| matches!(prop, FilterProp::HasSupertype { .. })),
            "supertype leaked to sibling leg: {:?}",
            second.properties
        );
    }
}
