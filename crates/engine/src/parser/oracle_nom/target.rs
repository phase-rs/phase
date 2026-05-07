//! Target phrase combinators for Oracle text parsing.
//!
//! Parses "target creature", "target creature or planeswalker you control", etc.
//! into typed `TargetFilter` values using nom 8.0 combinators.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::space1;
use nom::combinator::{opt, value};
use nom::sequence::preceded;
use nom::Parser;

use super::error::{OracleError, OracleResult};
use super::primitives::parse_color;
use crate::parser::oracle_util::parse_subtype;
use crate::types::ability::{ControllerRef, FilterProp, TargetFilter, TypeFilter, TypedFilter};
use crate::types::card_type::Supertype;
use crate::types::mana::ManaColor;

/// Parse a "target <type phrase>" from Oracle text.
///
/// Matches "target creature", "target artifact or enchantment you control", etc.
pub fn parse_target_phrase(input: &str) -> OracleResult<'_, TargetFilter> {
    preceded((tag("target"), space1), parse_type_phrase).parse(input)
}

/// Parse a type phrase into a `TargetFilter`.
///
/// Handles: optional "non" prefix, optional supertype, optional color prefix,
/// core type(s) joined by " or ", and optional controller suffix. This is the
/// nom equivalent of `oracle_target::parse_type_phrase`.
pub fn parse_type_phrase(input: &str) -> OracleResult<'_, TargetFilter> {
    // Optional "non" prefix (consumed separately from type negation)
    let (rest, non_prefix) = opt(parse_non_prefix).parse(input)?;

    // Optional supertype prefix ("legendary", "basic", "snow")
    let (rest, supertype_opt) = opt(parse_supertype_prefix).parse(rest)?;

    // Optional color prefix
    let (rest, color_opt) = opt(parse_color_prefix).parse(rest)?;

    // Core type(s) joined by " or "
    let (rest, types) = parse_type_list(rest)?;

    // Optional controller suffix
    let (rest, controller) = opt(preceded(space1, parse_controller_suffix)).parse(rest)?;

    let mut filter = build_type_filter(types, color_opt, supertype_opt, controller);

    // Wrap in Non if "non" prefix was present
    if non_prefix.is_some() {
        filter = match filter {
            TargetFilter::Typed(tf) => {
                if tf.type_filters.len() == 1 {
                    // Wrap the single type in Non
                    TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Non(Box::new(
                            tf.type_filters.into_iter().next().unwrap(),
                        ))],
                        controller: tf.controller,
                        properties: tf.properties,
                    })
                } else {
                    // Wrap the AnyOf in Non
                    TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Non(Box::new(TypeFilter::AnyOf(
                            tf.type_filters,
                        )))],
                        controller: tf.controller,
                        properties: tf.properties,
                    })
                }
            }
            other => other,
        };
    }

    Ok((rest, filter))
}

/// Parse a "non" prefix: "non" or "non-" followed by implicit word boundary.
fn parse_non_prefix(input: &str) -> OracleResult<'_, &str> {
    alt((tag("non-"), tag("non"))).parse(input)
}

/// Parse a supertype prefix ("legendary ", "basic ", "snow ") consuming trailing space.
pub fn parse_supertype_prefix(input: &str) -> OracleResult<'_, Supertype> {
    let (rest, st) = alt((
        value(Supertype::Legendary, tag("legendary")),
        value(Supertype::Basic, tag("basic")),
        value(Supertype::Snow, tag("snow")),
    ))
    .parse(input)?;
    let (rest, _) = space1.parse(rest)?;
    Ok((rest, st))
}

/// Parse a color word followed by a space, consuming both.
fn parse_color_prefix(input: &str) -> OracleResult<'_, ManaColor> {
    let (rest, c) = parse_color(input)?;
    let (rest, _) = space1.parse(rest)?;
    Ok((rest, c))
}

/// Parse a controller suffix: "you control", "an opponent controls",
/// "target player controls".
///
/// CR 109.4 + CR 115.1: "target player controls" generates a filter referencing
/// a chosen player target; the enclosing ability must surface a companion
/// TargetFilter::Player slot so the player is selected as part of target
/// declaration.
pub fn parse_controller_suffix(input: &str) -> OracleResult<'_, ControllerRef> {
    alt((
        value(ControllerRef::You, tag("you control")),
        value(ControllerRef::Opponent, tag("an opponent controls")),
        value(ControllerRef::Opponent, tag("your opponents control")),
        value(ControllerRef::TargetPlayer, tag("target player controls")),
    ))
    .parse(input)
}

/// Parse a list of type filters joined by " or ".
fn parse_type_list(input: &str) -> OracleResult<'_, Vec<TypeFilter>> {
    let (rest, first) = parse_type_filter_word(input)?;
    let mut types = vec![first];

    let mut remaining = rest;
    loop {
        if let Ok((r, _)) = tag::<_, _, OracleError<'_>>(" or ").parse(remaining) {
            if let Ok((r2, t)) = parse_type_filter_word(r) {
                types.push(t);
                remaining = r2;
                continue;
            }
        }
        break;
    }

    Ok((remaining, types))
}

/// Parse a single type filter word (singular or plural).
///
/// Uses a manual lookup for core/card types to avoid deep nom `alt` nesting which causes
/// stack overflow in debug builds, then falls back to the shared subtype table.
pub fn parse_type_filter_word(input: &str) -> OracleResult<'_, TypeFilter> {
    // Table of (prefix, TypeFilter) — longest-match-first within shared prefixes.
    static TYPE_WORDS: &[(&str, TypeFilter)] = &[
        ("creatures", TypeFilter::Creature),
        ("creature", TypeFilter::Creature),
        ("artifacts", TypeFilter::Artifact),
        ("artifact", TypeFilter::Artifact),
        ("enchantments", TypeFilter::Enchantment),
        ("enchantment", TypeFilter::Enchantment),
        ("instants", TypeFilter::Instant),
        ("instant", TypeFilter::Instant),
        ("sorceries", TypeFilter::Sorcery),
        ("sorcery", TypeFilter::Sorcery),
        ("planeswalkers", TypeFilter::Planeswalker),
        ("planeswalker", TypeFilter::Planeswalker),
        ("lands", TypeFilter::Land),
        ("land", TypeFilter::Land),
        ("battle", TypeFilter::Battle),
        ("permanents", TypeFilter::Permanent),
        ("permanent", TypeFilter::Permanent),
        ("cards", TypeFilter::Card),
        ("card", TypeFilter::Card),
        // "spell"/"spells" → Card per existing parser convention (CR 108.1)
        ("spells", TypeFilter::Card),
        ("spell", TypeFilter::Card),
    ];

    for &(word, ref tf) in TYPE_WORDS {
        if let Some(rest) = input.strip_prefix(word) {
            return Ok((rest, tf.clone()));
        }
    }

    if let Some((subtype, consumed)) = parse_subtype(input) {
        return Ok((&input[consumed..], TypeFilter::Subtype(subtype)));
    }

    Err(nom::Err::Error(nom::error::Error::new(
        input,
        nom::error::ErrorKind::Fail,
    )))
}

/// Parse a self-reference from Oracle text: "~", "it", "this creature",
/// "this permanent", "this spell", "this enchantment", "this artifact".
///
/// Returns `TargetFilter::SelfRef` when a self-reference is recognized.
pub fn parse_self_reference(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        value(TargetFilter::SelfRef, tag("~")),
        parse_it_self_reference,
        value(TargetFilter::SelfRef, tag("this creature")),
        value(TargetFilter::SelfRef, tag("this permanent")),
        value(TargetFilter::SelfRef, tag("this spell")),
        value(TargetFilter::SelfRef, tag("this enchantment")),
        value(TargetFilter::SelfRef, tag("this artifact")),
        value(TargetFilter::SelfRef, tag("this land")),
        value(TargetFilter::SelfRef, tag("this attraction")),
    ))
    .parse(input)
}

/// Parse "it" as a self-reference, requiring a word boundary after "it"
/// to prevent false matches on words like "item", "iterate".
fn parse_it_self_reference(input: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, _) = tag("it").parse(input)?;
    match rest.chars().next() {
        None | Some(' ' | ',' | ';' | '.' | ':' | ')' | '/' | '\'' | '"') => {
            Ok((rest, TargetFilter::SelfRef))
        }
        _ => Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        ))),
    }
}

/// Parse an event context reference from Oracle text.
///
/// Matches "that spell", "that player", "that creature", "defending player",
/// "the defending player", "that card", "that permanent".
/// Returns a `TargetFilter` for the referenced entity.
pub fn parse_event_context_ref(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        // Longest-match-first: "that spell's controller" before "that spell"
        value(
            TargetFilter::TriggeringSpellController,
            tag("that spell's controller"),
        ),
        value(
            TargetFilter::TriggeringSpellOwner,
            tag("that spell's owner"),
        ),
        value(TargetFilter::TriggeringSource, tag("that spell")),
        value(TargetFilter::TriggeringSource, tag("that creature")),
        value(TargetFilter::TriggeringSource, tag("that permanent")),
        value(TargetFilter::TriggeringSource, tag("that card")),
        value(TargetFilter::TriggeringPlayer, tag("that player")),
        // CR 506.3d: "defending player" / "the defending player"
        value(TargetFilter::DefendingPlayer, tag("the defending player")),
        value(TargetFilter::DefendingPlayer, tag("defending player")),
        // CR 608.2k: "the player" in trigger context is synonymous with
        // "that player" — anaphoric reference to the triggering player.
        // Ordered after "the defending player" so longest-match-first is
        // preserved for the specific defending-player phrasing.
        value(TargetFilter::TriggeringPlayer, tag("the player")),
    ))
    .parse(input)
}

/// Build a `TargetFilter` from parsed components.
fn build_type_filter(
    types: Vec<TypeFilter>,
    color: Option<ManaColor>,
    supertype: Option<Supertype>,
    controller: Option<ControllerRef>,
) -> TargetFilter {
    let type_filters: Vec<TypeFilter> = if types.len() == 1 {
        types
    } else {
        vec![TypeFilter::AnyOf(types)]
    };

    let mut properties = Vec::new();
    if let Some(c) = color {
        properties.push(FilterProp::HasColor { color: c });
    }
    if let Some(st) = supertype {
        properties.push(FilterProp::HasSupertype { value: st });
    }

    TargetFilter::Typed(TypedFilter {
        type_filters,
        controller,
        properties,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_target_phrase_creature() {
        let (rest, filter) = parse_target_phrase("target creature with power").unwrap();
        assert_eq!(rest, " with power");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn test_parse_target_phrase_artifact_or_enchantment() {
        let (rest, filter) =
            parse_target_phrase("target artifact or enchantment you control").unwrap();
        assert_eq!(rest, "");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(
                    tf.type_filters,
                    vec![TypeFilter::AnyOf(vec![
                        TypeFilter::Artifact,
                        TypeFilter::Enchantment
                    ])]
                );
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn test_parse_target_phrase_no_target_prefix() {
        assert!(parse_target_phrase("creature").is_err());
    }

    #[test]
    fn test_parse_controller_suffix() {
        let (rest, c) = parse_controller_suffix("you control stuff").unwrap();
        assert_eq!(c, ControllerRef::You);
        assert_eq!(rest, " stuff");

        let (rest2, c2) = parse_controller_suffix("an opponent controls").unwrap();
        assert_eq!(c2, ControllerRef::Opponent);
        assert_eq!(rest2, "");
    }

    #[test]
    fn test_parse_type_phrase_single() {
        let (rest, filter) = parse_type_phrase("creature you control").unwrap();
        assert_eq!(rest, "");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn test_parse_type_phrase_multi() {
        let (rest, filter) = parse_type_phrase("instant or sorcery").unwrap();
        assert_eq!(rest, "");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(
                    tf.type_filters,
                    vec![TypeFilter::AnyOf(vec![
                        TypeFilter::Instant,
                        TypeFilter::Sorcery
                    ])]
                );
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn test_parse_type_phrase_with_color() {
        let (rest, filter) = parse_type_phrase("white creature").unwrap();
        assert_eq!(rest, "");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                assert!(tf.properties.contains(&FilterProp::HasColor {
                    color: ManaColor::White
                }));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn test_parse_type_phrase_with_supertype() {
        let (rest, filter) = parse_type_phrase("legendary creature").unwrap();
        assert_eq!(rest, "");
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                assert!(tf.properties.contains(&FilterProp::HasSupertype {
                    value: Supertype::Legendary
                }));
            }
            _ => panic!("expected Typed filter"),
        }
    }

    #[test]
    fn test_parse_type_phrase_nonland() {
        // "nonland" → Non(Land) with trailing text unconsumed
        let (rest, filter) = parse_type_phrase("nonland permanent").unwrap();
        // The parser reads "non" prefix, then "land" as type, leaving " permanent"
        // It wraps the parsed type in Non
        match filter {
            TargetFilter::Typed(tf) => {
                assert_eq!(
                    tf.type_filters,
                    vec![TypeFilter::Non(Box::new(TypeFilter::Land))]
                );
            }
            _ => panic!("expected Typed filter"),
        }
        assert_eq!(rest, " permanent");
    }

    #[test]
    fn test_parse_self_reference() {
        let (rest, f) = parse_self_reference("~ gets").unwrap();
        assert_eq!(rest, " gets");
        assert_eq!(f, TargetFilter::SelfRef);

        let (rest2, f2) = parse_self_reference("it deals").unwrap();
        assert_eq!(rest2, " deals");
        assert_eq!(f2, TargetFilter::SelfRef);

        let (rest3, f3) = parse_self_reference("this creature gets").unwrap();
        assert_eq!(rest3, " gets");
        assert_eq!(f3, TargetFilter::SelfRef);
    }

    #[test]
    fn test_parse_self_reference_it_word_boundary() {
        // "item" should NOT match as "it" self-reference
        assert!(parse_self_reference("item").is_err());
        assert!(parse_self_reference("iterate").is_err());

        // "it" at end of input should match
        let (rest, f) = parse_self_reference("it").unwrap();
        assert_eq!(rest, "");
        assert_eq!(f, TargetFilter::SelfRef);
    }

    #[test]
    fn test_parse_event_context_ref() {
        let (rest, f) = parse_event_context_ref("that spell's controller gains").unwrap();
        assert_eq!(rest, " gains");
        assert_eq!(f, TargetFilter::TriggeringSpellController);

        let (rest2, f2) = parse_event_context_ref("that player loses").unwrap();
        assert_eq!(rest2, " loses");
        assert_eq!(f2, TargetFilter::TriggeringPlayer);

        let (rest3, f3) = parse_event_context_ref("defending player").unwrap();
        assert_eq!(rest3, "");
        assert_eq!(f3, TargetFilter::DefendingPlayer);

        let (rest4, f4) = parse_event_context_ref("that spell is countered").unwrap();
        assert_eq!(rest4, " is countered");
        assert_eq!(f4, TargetFilter::TriggeringSource);

        // CR 608.2k: "the player" in trigger context is anaphoric to
        // the triggering player (synonym for "that player").
        let (rest5, f5) = parse_event_context_ref("the player loses").unwrap();
        assert_eq!(rest5, " loses");
        assert_eq!(f5, TargetFilter::TriggeringPlayer);

        // "the defending player" still wins over "the player" (longest-match).
        let (rest6, f6) = parse_event_context_ref("the defending player gains").unwrap();
        assert_eq!(rest6, " gains");
        assert_eq!(f6, TargetFilter::DefendingPlayer);
    }

    #[test]
    fn test_parse_type_filter_word_plurals() {
        let r = parse_type_filter_word("creatures you");
        assert!(r.is_ok());
        let (rest, _t) = r.unwrap();
        assert_eq!(rest, " you");
    }

    #[test]
    fn test_parse_type_filter_word_spell() {
        // "spell" maps to Card per existing parser convention (CR 108.1)
        let (rest, t) = parse_type_filter_word("spell").unwrap();
        assert!(matches!(t, TypeFilter::Card), "expected Card for spell");
        assert_eq!(rest, "");
    }
}
