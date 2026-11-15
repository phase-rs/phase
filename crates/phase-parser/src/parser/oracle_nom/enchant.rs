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
use nom::combinator::{opt, value};
use nom::multi::many0;
use nom::sequence::preceded;
use nom::Parser;

use super::error::OracleResult;
use crate::parser::oracle_target::parse_without_keyword_suffix;
use crate::types::ability::{FilterProp, TargetFilter, TypedFilter};
pub(crate) use crate::types::text_grammar::{
    parse_enchant_attachment_qualifier, parse_enchant_controller_suffix,
    parse_enchant_qualified_type_leg, parse_enchant_type_leg,
};

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
pub(crate) fn parse_enchant_type_list(input: &str) -> OracleResult<'_, Vec<TypedFilter>> {
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
    let (input, type_legs) = parse_enchant_type_list(input)?;
    let (input, controller) = opt(parse_enchant_controller_suffix).parse(input)?;
    let (input, attachment) = opt(parse_enchant_attachment_qualifier).parse(input)?;
    let (input, without_keyword) = parse_enchant_without_keyword_suffix(input)?;

    let mut filters = Vec::with_capacity(type_legs.len());
    for mut typed in type_legs {
        if let Some(c) = controller.clone() {
            typed = typed.controller(c);
        }

        if let Some(prop) = attachment.clone() {
            typed.properties.push(prop);
        }
        typed.properties.extend(without_keyword.iter().cloned());

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
/// suffix authority so Aura legal-target sets match `parse_type_phrase_folding`.
fn parse_enchant_without_keyword_suffix(input: &str) -> OracleResult<'_, Vec<FilterProp>> {
    match parse_without_keyword_suffix(input) {
        Some((props, consumed)) => Ok((&input[consumed..], props)),
        None => Ok((input, Vec::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ControllerRef, TypeFilter};
    use crate::types::keywords::Keyword;

    // SHAPE: CR 702.5a + CR 205.3h: the exclusion belongs to the enchantment
    // leg, so an artifact Aura remains eligible through the artifact leg.
    #[test]
    fn puppet_crafting_negation_is_leg_local_shape() {
        let (rest, filter) = parse_enchant_target_full("artifact or non-aura enchantment")
            .expect("Puppet Crafting's complete Enchant clause must parse");
        assert!(rest.is_empty());
        let TargetFilter::Or { filters } = filter else {
            panic!("expected two Enchant legs");
        };
        assert_eq!(filters.len(), 2);
        let TargetFilter::Typed(artifact) = &filters[0] else {
            panic!("expected artifact leg");
        };
        assert_eq!(artifact.type_filters, vec![TypeFilter::Artifact]);
        let TargetFilter::Typed(enchantment) = &filters[1] else {
            panic!("expected enchantment leg");
        };
        assert_eq!(
            enchantment.type_filters,
            vec![
                TypeFilter::Enchantment,
                TypeFilter::Non(Box::new(TypeFilter::Subtype("Aura".into()))),
            ]
        );
    }

    // SHAPE: CR 702.5a: subtype adjectives compose with the established
    // supertype, controller, attachment and without-keyword axes.
    #[test]
    fn enchant_negated_subtype_composition_and_strict_remainder_shape() {
        use crate::types::card_type::Supertype;

        for (phrase, supertype, controller) in [
            ("non-wall creature", None, None),
            ("nonwall creature", None, None),
            (
                "legendary non-wall creature you control",
                Some(Supertype::Legendary),
                Some(ControllerRef::You),
            ),
        ] {
            let (rest, filter) = parse_enchant_target_full(phrase).unwrap();
            assert!(rest.is_empty());
            let TargetFilter::Typed(typed) = filter else {
                panic!("expected typed leg")
            };
            assert_eq!(
                typed.type_filters,
                vec![
                    TypeFilter::Creature,
                    TypeFilter::Non(Box::new(TypeFilter::Subtype("Wall".into())))
                ]
            );
            assert_eq!(typed.controller, controller);
            assert_eq!(
                typed.properties,
                supertype
                    .into_iter()
                    .map(|value| FilterProp::HasSupertype { value })
                    .collect::<Vec<_>>()
            );
        }
        let (rest, filter) = parse_enchant_target_full(
            "non-wall creature with another aura attached to it without flying",
        )
        .unwrap();
        assert!(rest.is_empty());
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected typed leg")
        };
        assert_eq!(typed.type_filters.len(), 2);
        assert!(typed
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::HasAttachment { .. })));
        assert!(typed.properties.iter().any(|p| matches!(
            p,
            FilterProp::WithoutKeyword {
                value: Keyword::Flying
            }
        )));
        for phrase in [
            "creature",
            "snow land you control",
            "creature or food",
            "vehicle or creature",
        ] {
            assert!(
                nom::combinator::all_consuming(parse_enchant_target_full)
                    .parse(phrase)
                    .is_ok(),
                "preservation: {phrase}"
            );
        }
        for phrase in [
            "",
            "non-Aurora enchantment",
            "non- enchantment",
            "non-aura",
            "nonartifact enchantment",
            "non-aura enchantment if you control a creature",
            "artifact or non-aura",
            "artifact non-aura enchantment",
            "goblin",
        ] {
            assert!(
                nom::combinator::all_consuming(parse_enchant_target_full)
                    .parse(phrase)
                    .is_err(),
                "must decline: {phrase}"
            );
        }
    }

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
