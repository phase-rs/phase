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
use crate::parser::oracle_util::{parse_subtype, OUTLAW_SUBTYPES};
use crate::types::ability::{ControllerRef, FilterProp, TargetFilter, TypeFilter, TypedFilter};
use crate::types::card_type::Supertype;
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

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

/// CR 205.4a: Parse a bare supertype word ("legendary", "basic", "snow")
/// without consuming any trailing boundary. Shared building block for both the
/// adjective-prefix form (`parse_supertype_prefix`, word + space) and trailing
/// relative-clause forms ("that aren't legendary", where the word is at
/// end-of-string). Callers that need a boundary apply their own check.
pub fn parse_supertype_word(input: &str) -> OracleResult<'_, Supertype> {
    alt((
        value(Supertype::Legendary, tag("legendary")),
        value(Supertype::Basic, tag("basic")),
        value(Supertype::Snow, tag("snow")),
    ))
    .parse(input)
}

/// Parse a supertype prefix ("legendary ", "basic ", "snow ") consuming trailing space.
pub fn parse_supertype_prefix(input: &str) -> OracleResult<'_, Supertype> {
    let (rest, st) = parse_supertype_word(input)?;
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
        // CR 112.1: a spell is a card on the stack — "spell"/"spells" → Card.
        ("spells", TypeFilter::Card),
        ("spell", TypeFilter::Card),
    ];

    // CR 700.12: "outlaw"/"outlaws" is a head noun for the Assassin, Mercenary,
    // Pirate, Rogue, and/or Warlock creature types. Tried before the bare-prefix
    // TYPE_WORDS scan and the subtype table because it expands to a disjunction
    // rather than a single subtype. The word-boundary guard prevents "outlawry"
    // (and similar prefixed words) from matching.
    if let Ok((rest, tf)) = parse_outlaw_type(input) {
        return Ok((rest, tf));
    }

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

/// CR 700.12: Parse the "outlaw"/"outlaws" head noun into a disjunction of the
/// Assassin, Mercenary, Pirate, Rogue, and Warlock creature types. Matches the
/// plural form first (longest-match), then requires a non-alphanumeric word
/// boundary so words like "outlawry" never match.
fn parse_outlaw_type(input: &str) -> OracleResult<'_, TypeFilter> {
    let (rest, _) = alt((tag("outlaws"), tag("outlaw"))).parse(input)?;
    match rest.chars().next() {
        // Word boundary: end of input or non-alphanumeric follower.
        None => {}
        Some(c) if !c.is_alphanumeric() => {}
        _ => {
            return Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Fail,
            )))
        }
    }
    let any_of = OUTLAW_SUBTYPES
        .iter()
        .map(|s| TypeFilter::Subtype((*s).to_string()))
        .collect();
    Ok((rest, TypeFilter::AnyOf(any_of)))
}

/// Parse a self-reference from Oracle text: "~", "it", "itself",
/// "this creature", "this permanent", "this spell", "this enchantment",
/// "this artifact".
///
/// Returns `TargetFilter::SelfRef` when a self-reference is recognized.
pub fn parse_self_reference(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        value(TargetFilter::SelfRef, tag("~")),
        parse_it_self_reference,
        // CR 201.5: "itself" is a self-reference to the object the ability is on.
        parse_itself_self_reference,
        value(TargetFilter::SelfRef, tag("this creature")),
        value(TargetFilter::SelfRef, tag("this permanent")),
        value(TargetFilter::SelfRef, tag("this spell")),
        value(TargetFilter::SelfRef, tag("this card")),
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

/// Parse "itself" as a self-reference, requiring a word boundary after "itself"
/// to prevent false matches on words like "itselfless".
fn parse_itself_self_reference(input: &str) -> OracleResult<'_, TargetFilter> {
    let (rest, _) = tag("itself").parse(input)?;
    match rest.chars().next() {
        None => Ok((rest, TargetFilter::SelfRef)),
        Some(c) if !c.is_alphanumeric() => Ok((rest, TargetFilter::SelfRef)),
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

/// Parse a "stack-object" target phrase — the disjunction of spells and/or
/// activated/triggered abilities currently on the stack that a counter effect
/// (or a retarget effect) can name as a target.
///
/// CR 701.6a: "To counter a spell or ability means to cancel it…" — the legal
/// target set of a counter effect is one of: spells on the stack, abilities on
/// the stack, or both. CR 113.3b/113.3c: activated and triggered abilities are
/// the two kinds of ability that exist as objects on the stack and can be
/// countered. CR 115.1: the target is chosen from the legal set the effect
/// defines, so the parser must reproduce that legal set faithfully — including
/// any type restriction on the spell disjunct ("noncreature spell").
///
/// Handles the full three-way disjunction "activated ability, triggered
/// ability, or noncreature spell" (Louisoix's Sacrifice) by composing two
/// independent axes:
///   1. the ability-kind phrase — "activated ability, triggered ability",
///      "activated or triggered ability", or "activated ability"; and
///   2. an optional ", or <type> spell" / "or <type> spell" tail describing a
///      restricted spell disjunct (e.g. "noncreature spell").
///
/// Also recognizes the "spell or ability" / "spell and/or ability" /
/// "ability or spell" form used by other counter cards →
/// `Or{StackSpell, StackAbility}`.
///
/// Deliberately does NOT match a phrase that is purely a spell type
/// restriction with no ability disjunct ("noncreature spell", "artifact or
/// enchantment spell", plain "spell"): those are already handled by
/// `parse_target` + `constrain_filter_to_stack`. This combinator only fires for
/// the cases bare `parse_target` cannot — an "activated/triggered ability"
/// disjunct. It returns a nom `Err` otherwise so callers fall back cleanly.
pub fn parse_stack_object_target(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        // "spell or ability" / "spell and/or ability" → both spells and abilities.
        value(
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::StackSpell,
                    TargetFilter::StackAbility { controller: None },
                ],
            },
            alt((
                tag("spell or ability"),
                tag("spell and/or ability"),
                tag("ability or spell"),
            )),
        ),
        // Ability-kind phrase, optionally followed by an "[,] or <type> spell"
        // tail. The two axes are composed independently rather than enumerated.
        parse_ability_kind_with_optional_spell,
    ))
    .parse(input)
}

/// Parse the ability-kind disjunct of a stack-object phrase, optionally
/// followed by a trailing spell disjunct.
///
/// CR 113.3b/113.3c: the only ability kinds that exist on the stack are
/// activated and triggered abilities; both map to `StackAbility { None }`
/// (the ability-kind axis carries no type, so the three spellings collapse to
/// one filter). An optional ", or <type> spell" / " or <type> spell" tail adds
/// the restricted spell disjunct, producing the `Or` for Louisoix's Sacrifice.
fn parse_ability_kind_with_optional_spell(input: &str) -> OracleResult<'_, TargetFilter> {
    // Axis 1: the ability-kind phrase. All spellings denote the same legal set
    // (any activated/triggered ability on the stack) — longest-match-first so
    // the comma-separated form is consumed whole before the shorter alternates.
    let (rest, _) = alt((
        tag("activated ability, triggered ability"),
        tag("activated or triggered ability"),
        tag("triggered or activated ability"),
        tag("triggered ability or activated ability"),
        tag("activated ability or triggered ability"),
        tag("triggered ability"),
        tag("activated ability"),
    ))
    .parse(input)?;

    // Axis 2 (optional): a trailing spell disjunct. The connector is "[,] or "
    // (Oracle uses a serial comma in the three-way list).
    let (rest, spell_leg) = opt(preceded(
        alt((tag(", or "), tag(" or "), tag(", "))),
        parse_restricted_spell,
    ))
    .parse(rest)?;

    let ability = TargetFilter::StackAbility { controller: None };
    let filter = match spell_leg {
        Some(spell) => TargetFilter::Or {
            filters: vec![ability, spell],
        },
        None => ability,
    };
    Ok((rest, filter))
}

/// Parse a (possibly type-restricted) spell phrase into a stack-constrained
/// `Typed` filter.
///
/// CR 112.1: a "spell" is a card (or copy of a card) on the stack. The leading
/// type phrase ("noncreature", "instant or sorcery", a bare "spell") is parsed
/// with the shared `parse_type_phrase` combinator, then the result is pinned to
/// the stack with an `InZone { Stack }` property so the runtime resolves it
/// against stack objects rather than the battlefield.
fn parse_restricted_spell(input: &str) -> OracleResult<'_, TargetFilter> {
    // `parse_type_phrase` consumes the leading type words. The phrase MUST
    // describe a spell: either it ends in an explicit " spell" noun (e.g.
    // "noncreature spell", "instant or sorcery spell") or `parse_type_phrase`
    // mapped a bare "spell" → `TypeFilter::Card`. Requiring this prevents the
    // combinator from swallowing battlefield type phrases ("creature you
    // control") as if they were stack spells.
    let (rest, filter) = parse_type_phrase(input)?;
    let is_bare_spell = matches!(
        &filter,
        TargetFilter::Typed(TypedFilter { type_filters, .. })
            if type_filters.as_slice() == [TypeFilter::Card]
    );
    let rest = match tag::<_, _, OracleError<'_>>(" spell").parse(rest) {
        Ok((r, _)) => r,
        Err(e) => {
            if is_bare_spell {
                // Phrase was just "spell" — already a spell, nothing to consume.
                rest
            } else {
                // A type phrase with no "spell" noun is not a spell phrase.
                return Err(e);
            }
        }
    };
    Ok((rest, constrain_typed_to_stack(filter)))
}

/// Add an `InZone { Stack }` property to a `Typed` filter so it resolves
/// against stack objects. Mirrors `oracle_effect::constrain_filter_to_stack`
/// but operates on the `oracle_nom` layer for the stack-object combinator.
fn constrain_typed_to_stack(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            mut properties,
        }) => {
            if !properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
            {
                properties.push(FilterProp::InZone { zone: Zone::Stack });
            }
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller,
                properties,
            })
        }
        other => other,
    }
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

        // "this card" used when the ability source is in a non-battlefield zone
        // (e.g. Ichorid: "other than this card from your graveyard").
        let (rest4, f4) = parse_self_reference("this card from your graveyard").unwrap();
        assert_eq!(rest4, " from your graveyard");
        assert_eq!(f4, TargetFilter::SelfRef);
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
    fn test_parse_self_reference_itself() {
        // "itself" at end of input should match
        let (rest, f) = parse_self_reference("itself").unwrap();
        assert_eq!(rest, "");
        assert_eq!(f, TargetFilter::SelfRef);

        // "itself" followed by word boundary should match
        let (rest2, f2) = parse_self_reference("itself.").unwrap();
        assert_eq!(rest2, ".");
        assert_eq!(f2, TargetFilter::SelfRef);

        let (rest3, f3) = parse_self_reference("itself-damage").unwrap();
        assert_eq!(rest3, "-damage");
        assert_eq!(f3, TargetFilter::SelfRef);

        // "itselfless" should NOT match as an "itself" self-reference.
        assert!(parse_self_reference("itselfless").is_err());
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
        // CR 112.1: a spell is a card on the stack — "spell" maps to Card.
        let (rest, t) = parse_type_filter_word("spell").unwrap();
        assert!(matches!(t, TypeFilter::Card), "expected Card for spell");
        assert_eq!(rest, "");
    }

    /// CR 700.12: the expected outlaw disjunction (Assassin, Mercenary, Pirate,
    /// Rogue, Warlock) produced by the "outlaw[s]" head noun.
    fn outlaw_any_of() -> TypeFilter {
        TypeFilter::AnyOf(
            ["Assassin", "Mercenary", "Pirate", "Rogue", "Warlock"]
                .iter()
                .map(|s| TypeFilter::Subtype((*s).to_string()))
                .collect(),
        )
    }

    #[test]
    fn test_parse_type_filter_word_outlaws() {
        // CR 700.12: "outlaws" expands to the five outlaw creature types.
        let (rest, t) = parse_type_filter_word("outlaws you control").unwrap();
        assert_eq!(t, outlaw_any_of());
        assert_eq!(rest, " you control");
    }

    #[test]
    fn test_parse_type_filter_word_outlaw_singular() {
        let (rest, t) = parse_type_filter_word("outlaw").unwrap();
        assert_eq!(t, outlaw_any_of());
        assert_eq!(rest, "");
    }

    #[test]
    fn test_parse_type_filter_word_outlawry_does_not_match_outlaw() {
        // Word-boundary guard: "outlawry" must NOT match the "outlaw" head noun.
        // An `Err` is also acceptable — "outlawry" is not a type word at all.
        if let Ok((_, tf)) = parse_type_filter_word("outlawry") {
            assert_ne!(
                tf,
                outlaw_any_of(),
                "outlawry must not parse as the outlaw disjunction"
            );
        }
    }

    // --- parse_stack_object_target (CR 701.6a + CR 115.1) ---

    /// The noncreature-spell disjunct: a stack-pinned `Typed` filter that
    /// excludes creature spells via `TypeFilter::Non(Creature)`.
    fn noncreature_spell_leg() -> TargetFilter {
        TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Non(Box::new(TypeFilter::Creature))],
            controller: None,
            properties: vec![FilterProp::InZone { zone: Zone::Stack }],
        })
    }

    #[test]
    fn test_stack_object_three_way_disjunction() {
        // Louisoix's Sacrifice — the full three-way disjunction.
        let (rest, filter) =
            parse_stack_object_target("activated ability, triggered ability, or noncreature spell")
                .unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            filter,
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::StackAbility { controller: None },
                    noncreature_spell_leg(),
                ],
            }
        );
    }

    #[test]
    fn test_stack_object_noncreature_excludes_creature_spell() {
        // The noncreature restriction must be carried as a typed `Non` leg —
        // a creature spell is NOT a member of the legal target set.
        let (_, filter) =
            parse_stack_object_target("activated ability, triggered ability, or noncreature spell")
                .unwrap();
        let TargetFilter::Or { filters } = &filter else {
            panic!("expected Or filter");
        };
        let spell_leg = &filters[1];
        let TargetFilter::Typed(tf) = spell_leg else {
            panic!("expected Typed spell leg");
        };
        assert_eq!(
            tf.type_filters,
            vec![TypeFilter::Non(Box::new(TypeFilter::Creature))],
            "creature spells must be excluded by the noncreature restriction"
        );
        assert!(
            tf.properties
                .contains(&FilterProp::InZone { zone: Zone::Stack }),
            "the spell leg must be pinned to the stack zone"
        );
    }

    #[test]
    fn test_stack_object_activated_or_triggered_ability() {
        // Ability-only counter (e.g. Stifle / Disallow's ability disjunct).
        let (rest, filter) = parse_stack_object_target("activated or triggered ability").unwrap();
        assert_eq!(rest, "");
        assert_eq!(filter, TargetFilter::StackAbility { controller: None });
    }

    #[test]
    fn test_stack_object_activated_ability_only() {
        let (rest, filter) = parse_stack_object_target("activated ability").unwrap();
        assert_eq!(rest, "");
        assert_eq!(filter, TargetFilter::StackAbility { controller: None });
    }

    #[test]
    fn test_stack_object_spell_or_ability() {
        // Disallow / Voidslime — "counter target spell, activated ability, or
        // triggered ability" reduces (in the simple two-way form) to the
        // "spell or ability" phrasing.
        let (rest, filter) = parse_stack_object_target("spell or ability").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            filter,
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::StackSpell,
                    TargetFilter::StackAbility { controller: None },
                ],
            }
        );
    }

    #[test]
    fn test_stack_object_rejects_pure_spell_phrase() {
        // A phrase that is purely a spell type restriction (no ability
        // disjunct) must NOT be matched — `parse_target` handles those.
        assert!(parse_stack_object_target("noncreature spell").is_err());
        assert!(parse_stack_object_target("artifact or enchantment spell").is_err());
        assert!(parse_stack_object_target("spell").is_err());
        // A battlefield type phrase must likewise not be swallowed.
        assert!(parse_stack_object_target("creature you control").is_err());
        assert!(parse_stack_object_target("permanent").is_err());
    }
}
