use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::{tag, take_till};
use nom::combinator::{opt, value};
use nom::Parser;

use crate::types::ability::{
    AttachmentKind, ControllerRef, FilterProp, QuantityExpr, QuantityRef, SharedQuality,
    TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::Supertype;
use crate::types::identifiers::TrackedSetId;
use crate::types::keywords::{Keyword, KeywordKind};
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

use super::oracle_nom::filter as nom_filter;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::quantity as nom_quantity;
use super::oracle_nom::target as nom_target;
use super::oracle_quantity::capitalize_first;
use super::oracle_target_scope;
use super::oracle_util::{
    merge_or_filters, parse_subtype, strip_possessive, TextPair, SELF_REF_PARSE_ONLY_PHRASES,
    SELF_REF_TYPE_PHRASES,
};
use super::oracle_warnings::push_warning;

/// Run a nom combinator on lowercased text, returning the result and
/// remainder from the original (mixed-case) text.
///
/// This bridges the nom combinator world (which expects lowercase input)
/// with the oracle_target API (which preserves original casing in remainders).
fn nom_on_lower<'a, T, F>(text: &'a str, lower: &str, mut parser: F) -> Option<(T, &'a str)>
where
    F: FnMut(&str) -> super::oracle_nom::error::OracleResult<'_, T>,
{
    let (rest, result) = parser(lower).ok()?;
    let consumed = lower.len() - rest.len();
    Some((result, &text[consumed..]))
}

/// Parse a word with a word boundary check: the next char after the word must be
/// non-alphanumeric (whitespace, comma, period, etc.) or end-of-input.
/// Prevents "it" from matching "item", "you" from matching "your", etc.
fn parse_word_bounded<'a>(
    input: &'a str,
    word: &str,
) -> super::oracle_nom::error::OracleResult<'a, ()> {
    let (rest, _) = tag::<_, _, nom_language::error::VerboseError<&str>>(word).parse(input)?;
    match rest.chars().next() {
        None | Some(' ' | ',' | '.' | ';' | ':' | ')' | '\'' | '"' | '/' | '-') => Ok((rest, ())),
        _ => Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Context("word boundary required"),
            )],
        })),
    }
}

/// Parse an event-context possessive reference from Oracle text.
/// These resolve from the triggering event, not from player targeting.
/// Must be checked BEFORE standard `parse_target` for trigger-based effects.
/// CR 608.2k: Parse event-context references ("that player", "that permanent", etc.)
/// that refer back to objects/players mentioned in a trigger condition or cost.
/// Returns the matched filter and unconsumed remainder text.
pub fn parse_event_context_ref(text: &str) -> Option<(TargetFilter, &str)> {
    let text = text.trim();
    let lower = text.to_lowercase();

    // CR 608.2k: Event-context references resolve from the triggering event.
    // All patterns in one nom alt() for clean longest-match-first dispatch.
    nom_on_lower(text, &lower, |input| {
        nom::branch::alt((
            // Longest-match-first within shared prefixes.
            value(
                TargetFilter::TriggeringSpellController,
                tag::<_, _, nom_language::error::VerboseError<&str>>("that spell's controller"),
            ),
            value(
                TargetFilter::TriggeringSpellOwner,
                tag("that spell's owner"),
            ),
            // CR 608.2c: "its controller" / "their controller" — controller of the parent target.
            value(TargetFilter::ParentTargetController, tag("its controller")),
            value(
                TargetFilter::ParentTargetController,
                tag("their controller"),
            ),
            value(TargetFilter::TriggeringPlayer, tag("that player")),
            value(TargetFilter::TriggeringSource, tag("that source")),
            // "that permanent or player" before "that permanent" — longest match first.
            value(
                TargetFilter::TriggeringSource,
                tag("that permanent or player"),
            ),
            value(TargetFilter::TriggeringSource, tag("that permanent")),
            // CR 608.2k + CR 301.5a: "that creature" inside a trigger refers to the
            // triggering source object (e.g. Pip-Boy 3000's "Whenever equipped
            // creature attacks ... put a +1/+1 counter on that creature"), not to
            // a parent target. Placed after longer "that ..." phrases so
            // longest-match-first dispatch is preserved.
            value(TargetFilter::TriggeringSource, tag("that creature")),
            // CR 506.3d: "defending player" — the player being attacked.
            value(TargetFilter::DefendingPlayer, tag("defending player")),
        ))
        .parse(input)
    })
}

/// Parse a target description from Oracle text, returning (filter, remaining_text).
/// Consumes the longest matching target phrase.
///
/// Uses first-character dispatch to group `starts_with` checks, reducing average
/// comparisons from ~12 to ~3-4 per call.
pub fn parse_target(text: &str) -> (TargetFilter, &str) {
    let text = text.trim_start();
    let lower = text.to_lowercase();

    // Strip leading article ("a "/"an ") before "target" to handle "a target creature".
    // Guard: only strip when followed by "target " to avoid over-stripping.
    if let Ok((after_article, _)) = alt((
        tag::<_, _, nom_language::error::VerboseError<&str>>("a "),
        tag("an "),
    ))
    .parse(lower.as_str())
    {
        if after_article.starts_with("target ") {
            let original_rest = &text[lower.len() - after_article.len()..];
            return parse_target(original_rest);
        }
    }

    // Quantified target phrases routed here from callers that only need the filter,
    // not the target-count metadata.
    static QUANTIFIED_PREFIXES: &[&str] = &[
        "any number of ",
        "up to x ",
        "up to one ",
        "up to two ",
        "up to three ",
        "up to four ",
        "up to five ",
        "up to six ",
        "one, two, or three ",
        "one or two ",
        "one ",
        "two ",
        "three ",
        "four ",
        "five ",
        "six ",
        "x ",
    ];
    for prefix in QUANTIFIED_PREFIXES {
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(*prefix).parse(lower.as_str())
        {
            let trimmed_rest = rest.trim_start();
            let quantified_target = alt((
                tag::<_, _, nom_language::error::VerboseError<&str>>("target "),
                tag("other target "),
                tag("another target "),
                tag("other "),
            ))
            .parse(rest)
            .is_ok()
                || starts_with_type_word(trimmed_rest)
                || starts_with_type_phrase_lead(trimmed_rest)
                || parse_combat_status_prefix(trimmed_rest).is_some()
                // Pronoun references after quantity: "any number of them"
                || parse_word_bounded(trimmed_rest, "them").is_ok()
                || parse_word_bounded(trimmed_rest, "it").is_ok()
                || (!matches!(*prefix, "one " | "up to one ") && trimmed_rest.starts_with("of "));
            if quantified_target {
                let original_rest = &text[lower.len() - rest.len()..];
                return parse_target(original_rest);
            }
        }
    }

    for prefix in ["or untap ", "untap ", "or tap ", "tap "] {
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(prefix).parse(lower.as_str())
        {
            let original_rest = &text[lower.len() - rest.len()..];
            return parse_target(original_rest);
        }
    }

    for phrase in [
        "one, two, or three targets",
        "one or two targets",
        "any number of targets",
        "targets",
    ] {
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(phrase).parse(lower.as_str())
        {
            return (TargetFilter::Any, &text[lower.len() - rest.len()..]);
        }
    }

    // CR 608.2c: Bare anaphoric references inherit the parent target selected earlier
    // in the same spell/ability instruction sequence.
    // "it" with word boundary — prevents matching "item", "iterate", etc.
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| parse_word_bounded(input, "it")) {
        return (TargetFilter::ParentTarget, rest);
    }
    // "them" with word boundary
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| parse_word_bounded(input, "them")) {
        return (TargetFilter::ParentTarget, rest);
    }
    if tag::<_, _, nom_language::error::VerboseError<&str>>("one of ")
        .parse(lower.as_str())
        .is_err()
    {
        if let Some((_, rest)) =
            nom_on_lower(text, &lower, |input| parse_word_bounded(input, "one"))
        {
            return (TargetFilter::ParentTarget, rest);
        }
    }
    // Gendered object pronouns also refer back to the prior selected object.
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| parse_word_bounded(input, "him")) {
        return (TargetFilter::ParentTarget, rest);
    }
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| parse_word_bounded(input, "her")) {
        return (TargetFilter::ParentTarget, rest);
    }
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("on ").parse(lower.as_str())
    {
        let original_rest = &text[lower.len() - rest.len()..];
        if matches!(
            rest,
            "it" | "them" | "him" | "her" | "enchanted permanent" | "enchanted creature"
        ) {
            return parse_target(original_rest);
        }
    }
    // "that [type phrase]" → anaphoric reference to a typed subject
    if let Ok((rest_subject, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("that ").parse(lower.as_str())
    {
        let original_rest = &text[lower.len() - rest_subject.len()..];
        let (filter, rem) = parse_type_phrase(original_rest);
        if !matches!(filter, TargetFilter::Any) {
            return (TargetFilter::ParentTarget, rem);
        }
    }

    // "~" — self-reference (normalized from card name)
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("~").parse(lower.as_str())
    {
        return (
            TargetFilter::SelfRef,
            text[lower.len() - rest.len()..].trim_start(),
        );
    }

    // "any other target" — matches any legal target different from previously chosen targets
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| {
        value(
            (),
            tag::<_, _, nom_language::error::VerboseError<&str>>("any other target"),
        )
        .parse(input)
    }) {
        return (
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Another])),
            rest,
        );
    }

    // "any target" — matches any legal target
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| {
        value(
            TargetFilter::Any,
            tag::<_, _, nom_language::error::VerboseError<&str>>("any target"),
        )
        .parse(input)
    }) {
        return (TargetFilter::Any, rest);
    }

    // "all " + type phrase
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("all ").parse(lower.as_str())
    {
        let (filter, rest) = parse_type_phrase(&text[lower.len() - rest.len()..]);
        return (filter, rest);
    }

    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| parse_word_bounded(input, "player"))
    {
        return (TargetFilter::Player, rest);
    }

    for zone_word in ["graveyard", "graveyards"] {
        if let Some((_, rest)) =
            nom_on_lower(text, &lower, |input| parse_word_bounded(input, zone_word))
        {
            return (
                TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                    zone: Zone::Graveyard,
                }])),
                rest,
            );
        }
    }

    // CR 201.5: "this creature", "this spell", etc. — self-reference
    for phrase in SELF_REF_TYPE_PHRASES
        .iter()
        .chain(SELF_REF_PARSE_ONLY_PHRASES)
    {
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(*phrase).parse(lower.as_str())
        {
            return (TargetFilter::SelfRef, &text[lower.len() - rest.len()..]);
        }
    }

    // "target" group — longest-match-first within
    if let Ok((after_target, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("target ").parse(lower.as_str())
    {
        let target_offset = lower.len() - after_target.len();
        // "target player or planeswalker"
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("player or planeswalker")
                .parse(after_target)
        {
            return (
                TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Player,
                        typed(TypeFilter::Planeswalker, None, vec![], vec![]),
                    ],
                },
                &text[lower.len() - rest.len()..],
            );
        }
        // "target opponent"
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("opponent").parse(after_target)
        {
            return (
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                &text[lower.len() - rest.len()..],
            );
        }
        // "target player"
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("player").parse(after_target)
        {
            return (TargetFilter::Player, &text[lower.len() - rest.len()..]);
        }
        // "target" + type phrase (generic)
        let (filter, rest) = parse_type_phrase(&text[target_offset..]);
        return (filter, rest);
    }

    // CR 603.7: Anaphoric tracked-set pronouns
    static TRACKED_SET_PHRASES: &[&str] = &[
        "the chosen cards",
        "the rest",
        "the other",
        "those land cards",
        "those permanent cards",
        "those creature cards",
        "those lands",
        "those tokens",
        "those auras",
        "the revealed cards",
        "those cards",
        "those permanents",
        "those creatures",
        "the exiled cards",
        "the exiled card",
        "the exiled permanents",
        "the exiled permanent",
        "the exiled creature",
        "both creatures",
        // CR 608.2c: "later text on the card may modify the meaning of earlier
        // text" — anaphoric back-reference to objects produced by prior sibling
        // steps in the same resolution (e.g., Sword of Hearth and Home: exiled
        // creature + searched basic land → "Put both cards onto the battlefield
        // under your control").
        "both cards",
    ];
    for phrase in TRACKED_SET_PHRASES {
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(*phrase).parse(lower.as_str())
        {
            return (
                TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                &text[lower.len() - rest.len()..],
            );
        }
    }

    // Singular selection from a previously-referenced set.
    static SELECTED_FROM_SET_PHRASES: &[&str] = &[
        "new targets for the copies",
        "new targets for the copy",
        "new targets for that copy",
        "new targets for target spell",
        "new targets for it",
        "a new target for it",
        "up to one of them",
        "either of them",
        "the chosen creature",
        "the chosen card",
        "the chosen player",
        "the last chosen card",
        "the revealed card",
        "the token",
        "one of those cards",
        "one of those permanents",
        "one of those creatures",
        "one of the revealed cards",
        "one of them",
    ];
    for phrase in SELECTED_FROM_SET_PHRASES {
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(*phrase).parse(lower.as_str())
        {
            return (
                TargetFilter::ParentTarget,
                &text[lower.len() - rest.len()..],
            );
        }
    }

    // Set references that appear after an explicit quantity has already been parsed
    // upstream, e.g. "put two of them into your hand".
    static SET_REFERENCE_SUFFIXES: &[&str] = &[
        "of those cards",
        "of those permanents",
        "of those creatures",
        "of the revealed cards",
        "of them",
    ];
    for phrase in SET_REFERENCE_SUFFIXES {
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(*phrase).parse(lower.as_str())
        {
            return (
                TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                &text[lower.len() - rest.len()..],
            );
        }
    }

    // CR 608.2c: Definite anaphoric references to previously-mentioned objects/players.
    // Longest-match-first: "the creature's controller" before "the creature".
    if let Some((filter, rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(
                TargetFilter::ParentTargetController,
                tag::<_, _, nom_language::error::VerboseError<&str>>("the creature's controller"),
            ),
            value(
                TargetFilter::ParentTargetController,
                tag("the source's controller"),
            ),
            value(TargetFilter::ParentTargetController, tag("its controller")),
            value(TargetFilter::ParentTarget, tag("the player")),
            value(TargetFilter::ParentTarget, tag("the creature")),
            value(TargetFilter::ParentTarget, tag("the spell")),
            value(TargetFilter::ParentTarget, tag("the land")),
        ))
        .parse(input)
    }) {
        return (filter, rest);
    }
    // Generic "the [noun]'s controller" — any possessive ending in "'s controller"
    // catches subtypes like "the Wall's controller" and similar.
    if let Ok((after_the, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("the ").parse(lower.as_str())
    {
        if let Some(pos) = after_the.find("'s controller") {
            let consumed = "the ".len() + pos + "'s controller".len();
            return (TargetFilter::ParentTargetController, &text[consumed..]);
        }
    }
    // "the [type] card" / "the enchanted [type] card" — definite reference to a
    // previously-mentioned typed card. Must come after tracked-set phrases.
    if let Ok((after_the, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("the ").parse(lower.as_str())
    {
        // "the enchanted card" / "the enchanted instant card"
        let type_start = if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("enchanted ").parse(after_the)
        {
            rest
        } else {
            after_the
        };

        // Check for [type] card pattern: the remaining must start with a type word
        // followed by " card"/"cards", or just be "card"/"cards" directly.
        let has_type_card =
            if let Ok((after_type, _)) = nom_target::parse_type_filter_word(type_start) {
                let after_type = after_type.trim_start();
                after_type.starts_with("card") || after_type.is_empty()
            } else {
                false
            };

        // Also check bare "card"/"cards" (e.g., "the enchanted card")
        let is_bare_card = type_start.starts_with("card");

        if has_type_card || is_bare_card {
            // Find end of "card"/"cards"
            let card_start = if is_bare_card {
                type_start
            } else if let Ok((after_type, _)) = nom_target::parse_type_filter_word(type_start) {
                after_type.trim_start()
            } else {
                type_start
            };
            let rest_after_card = if let Ok((r, _)) =
                tag::<_, _, nom_language::error::VerboseError<&str>>("cards").parse(card_start)
            {
                r
            } else if let Ok((r, _)) =
                tag::<_, _, nom_language::error::VerboseError<&str>>("card").parse(card_start)
            {
                r
            } else {
                card_start
            };
            let consumed = lower.len() - rest_after_card.len();
            return (TargetFilter::ParentTarget, &text[consumed..]);
        }
    }
    // "himself" / "herself" — archaic self-reference (e.g., "deals damage to himself")
    if let Ok((rest, _)) = alt((
        tag::<_, _, nom_language::error::VerboseError<&str>>("himself"),
        tag("herself"),
    ))
    .parse(lower.as_str())
    {
        return (TargetFilter::SelfRef, &text[lower.len() - rest.len()..]);
    }

    // "each opponent" / "opponents" — opponent player references
    if let Some((filter, rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                tag::<_, _, nom_language::error::VerboseError<&str>>("each opponent"),
            ),
            value(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                tag("opponents"),
            ),
        ))
        .parse(input)
    }) {
        return (filter, rest);
    }

    for phrase in ["opponent's graveyard", "an opponent's graveyard"] {
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(phrase).parse(lower.as_str())
        {
            return (
                TargetFilter::Typed(TypedFilter::card().properties(vec![
                    FilterProp::Owned {
                        controller: ControllerRef::Opponent,
                    },
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ])),
                &text[lower.len() - rest.len()..],
            );
        }
    }

    // CR 610.3: "each card exiled with ~" / "each card exiled with this <type>"
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("each card exiled with ~")
            .parse(lower.as_str())
    {
        return (
            TargetFilter::ExiledBySource,
            &text[lower.len() - rest.len()..],
        );
    }
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("each card exiled with this ")
            .parse(lower.as_str())
    {
        // Skip the type word after "this " to consume "each card exiled with this artifact"
        let after_type = rest.find(' ').map_or("", |i| &rest[i..]);
        return (
            TargetFilter::ExiledBySource,
            &text[text.len() - after_type.len()..],
        );
    }

    // "each of those creatures/permanents/cards" → TrackedSet reference
    if let Ok((rest, _)) = alt((
        tag::<_, _, nom_language::error::VerboseError<&str>>("each of those creatures"),
        tag("each of those permanents"),
        tag("each of those cards"),
    ))
    .parse(lower.as_str())
    {
        return (
            TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
            &text[lower.len() - rest.len()..],
        );
    }

    // "each " + type phrase
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("each ").parse(lower.as_str())
    {
        let (filter, rest) = parse_type_phrase(&text[lower.len() - rest.len()..]);
        return (filter, rest);
    }

    // "enchanted [type]" / "equipped creature"
    // First check special case: "enchanted permanent's controller" → controller ref
    if let Some((filter, rest)) = nom_on_lower(text, &lower, |input| {
        value(
            TargetFilter::ParentTargetController,
            tag::<_, _, nom_language::error::VerboseError<&str>>(
                "enchanted permanent's controller",
            ),
        )
        .parse(input)
    }) {
        return (filter, rest);
    }
    // "enchanted [type phrase]" → parse the type after "enchanted " and add EnchantedBy
    if let Ok((rest_lower, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("enchanted ").parse(lower.as_str())
    {
        let after_enchanted = &text[lower.len() - rest_lower.len()..];
        let (filter, rest) = parse_type_phrase(after_enchanted);
        if target_filter_has_meaningful_content(&filter) {
            let enchanted = match filter {
                TargetFilter::Typed(mut tf) => {
                    tf.properties.push(FilterProp::EnchantedBy);
                    TargetFilter::Typed(tf)
                }
                other => other,
            };
            return (enchanted, rest);
        }
    }
    // "equipped creature" → creature with EquippedBy
    if let Some((filter, rest)) = nom_on_lower(text, &lower, |input| {
        value(
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy])),
            tag::<_, _, nom_language::error::VerboseError<&str>>("equipped creature"),
        )
        .parse(input)
    }) {
        return (filter, rest);
    }

    // "cards exiled with ~" / "cards exiled with this <type>"
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("cards exiled with ~")
            .parse(lower.as_str())
    {
        return (
            TargetFilter::ExiledBySource,
            &text[lower.len() - rest.len()..],
        );
    }
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("cards exiled with this ")
            .parse(lower.as_str())
    {
        let after_type = rest.find(' ').map_or("", |i| &rest[i..]);
        return (
            TargetFilter::ExiledBySource,
            &text[text.len() - after_type.len()..],
        );
    }

    // "you" — the controller (not a targeted player), with word boundary
    if let Some((_, rest)) = nom_on_lower(text, &lower, |input| parse_word_bounded(input, "you")) {
        return (TargetFilter::Controller, rest);
    }

    // "the top/bottom [N] [type] card[s] of [possessive] library/graveyard"
    // Zone position references that appear as targets of exile/mill/reveal effects.
    // Returns a filter with InZone for the referenced zone and controller.
    if let Some((filter, rest)) = parse_zone_position_ref(text, &lower) {
        return (filter, rest);
    }

    // CR 400.12: Bare possessive zone references ("their graveyard", "your library").
    // Effects targeting a zone act on all cards in that zone.
    // Skip "its owner's" — ControllerRef has no Owner variant; handle when needed.
    if let Some((poss, rest)) = strip_possessive(&lower) {
        if poss != "its owner's" {
            static ZONE_WORDS: &[(&str, Zone)] = &[
                ("graveyard", Zone::Graveyard),
                ("library", Zone::Library),
                ("hand", Zone::Hand),
            ];
            for &(zone_word, zone) in ZONE_WORDS {
                if let Ok((zone_rest, _)) =
                    tag::<_, _, nom_language::error::VerboseError<&str>>(zone_word).parse(rest)
                {
                    let consumed = lower.len() - zone_rest.len();
                    return (
                        TargetFilter::Typed(TypedFilter {
                            controller: Some(ControllerRef::You),
                            properties: vec![FilterProp::InZone { zone }],
                            ..Default::default()
                        }),
                        &text[consumed..],
                    );
                }
            }
        }
    }

    // CR 903.3: Possessive commander reference ("your commander" /
    // "their commander" / "your commanders"). The commander is identified by
    // the IsCommander flag, not by a creature subtype. Effects like Command
    // Beacon's "Put your commander into your hand from the command zone" need
    // a typed target carrying IsCommander + the controller scope so the
    // resolver can locate the right card.
    if let Some((_poss, rest)) = strip_possessive(&lower) {
        for word in &["commanders", "commander"] {
            if let Ok((after, _)) =
                tag::<_, _, nom_language::error::VerboseError<&str>>(*word).parse(rest)
            {
                let consumed = lower.len() - after.len();
                return (
                    TargetFilter::Typed(TypedFilter {
                        controller: Some(ControllerRef::You),
                        properties: vec![FilterProp::IsCommander],
                        ..Default::default()
                    }),
                    &text[consumed..],
                );
            }
        }
    }

    // Bare type phrase fallback: try parse_type_phrase before giving up.
    // Handles "other nonland permanents you own and control" after quantifier stripping.
    let (filter, rest) = parse_type_phrase(text);
    if target_filter_has_meaningful_content(&filter) {
        (filter, rest)
    } else {
        push_warning(format!(
            "target-fallback: parse_target could not classify '{}'",
            text.trim()
        ));
        (TargetFilter::Any, text)
    }
}

/// Parse a type phrase like "creature", "nonland permanent", "artifact or enchantment",
/// "creature you control", "creature an opponent controls".
pub fn parse_type_phrase(text: &str) -> (TargetFilter, &str) {
    let lower = text.to_lowercase();
    let mut pos = 0;
    let mut properties = Vec::new();
    let lower_trimmed = lower.trim_start();
    let offset = lower.len() - lower_trimmed.len();
    pos += offset;

    // Strip leading article ("a "/"an ") when followed by a recognized type word.
    // Guard: "an opponent" → "opponent" fails type word check → no stripping.
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("a ").parse(&lower[pos..])
    {
        if starts_with_type_phrase_lead(rest) {
            pos += "a ".len();
        }
    } else if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("an ").parse(&lower[pos..])
    {
        if starts_with_type_phrase_lead(rest) {
            pos += "an ".len();
        }
    }

    // Handle "other"/"another" prefix: "other creatures", "another creature",
    // "other nonland permanents", "another target creature"
    if tag::<_, _, nom_language::error::VerboseError<&str>>("other ")
        .parse(lower_trimmed)
        .is_ok()
    {
        properties.push(FilterProp::Another);
        pos = offset + "other ".len();
    } else if tag::<_, _, nom_language::error::VerboseError<&str>>("another ")
        .parse(lower_trimmed)
        .is_ok()
    {
        properties.push(FilterProp::Another);
        pos = offset + "another ".len();
    }
    // "another target [type]" — strip "target " after "another " so the type is reachable.
    if properties.contains(&FilterProp::Another) {
        if let Ok((_, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("target ").parse(&lower[pos..])
        {
            pos += "target ".len();
        }
    }

    // CR 509.1h: Consume combat status prefixes (unblocked, attacking, blocking).
    // Handles "or" compound: "attacking or blocking creature" → [Attacking, Blocking].
    while let Some((prop, consumed)) = parse_combat_status_prefix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
        // Check for "or " followed by another combat status prefix
        if let Ok((after_or, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("or ").parse(&lower[pos..])
        {
            if let Some((next_prop, next_consumed)) = parse_combat_status_prefix(after_or) {
                properties.push(next_prop);
                pos += "or ".len() + next_consumed;
            }
        }
    }

    // CR 205.4a: Parse supertype prefix: "legendary", "basic", "snow"
    // Must come BEFORE color prefix so "legendary white creature" works:
    // supertype consumed first, then color at the new position.
    if let Ok((rest, supertype)) = nom_target::parse_supertype_prefix(&lower[pos..]) {
        properties.push(FilterProp::HasSupertype { value: supertype });
        pos += lower[pos..].len() - rest.len();
    }

    // CR 303.4 + CR 301.5: "enchanted" / "equipped" attachment adjective prefix.
    // Attach the property; runtime evaluation degrades "EnchantedBy" to
    // "has any Aura attached" when the trigger source itself is not the Aura
    // (Hateful Eidolon). Source-relative sources (Auras, Equipment) retain the
    // CR 702.5a semantics via the same FilterProp.
    if let Ok((rest, prop)) = alt((
        value(
            FilterProp::EnchantedBy,
            tag::<_, _, nom_language::error::VerboseError<&str>>("enchanted "),
        ),
        value(
            FilterProp::EquippedBy,
            tag::<_, _, nom_language::error::VerboseError<&str>>("equipped "),
        ),
    ))
    .parse(&lower[pos..])
    {
        // Only consume if a type word follows (so "enchanted forest" also works,
        // as does "enchanted creature", but bare "enchanted" alone does not).
        if starts_with_type_phrase_lead(rest) {
            properties.push(prop);
            pos += lower[pos..].len() - rest.len();
        }
    }

    // CR 700.4 + CR 700.9: "modified" adjective prefix. A permanent is modified
    // if it has counters on it, is equipped, or is enchanted by an Aura its
    // controller controls. Emits FilterProp::Modified (a first-class typed
    // predicate — see `FilterProp::Modified` in types/ability.rs). Mirrors the
    // "enchanted " / "equipped " adjective handling above: only consume when a
    // type word follows, so bare "modified" alone doesn't hijack other
    // contexts.
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("modified ").parse(&lower[pos..])
    {
        if starts_with_type_phrase_lead(rest) {
            properties.push(FilterProp::Modified);
            pos += lower[pos..].len() - rest.len();
        }
    }

    // CR 700.6: "historic" adjective prefix. An object is historic if it has
    // the legendary supertype, the artifact card type, or the Saga subtype.
    // Emits FilterProp::Historic (a first-class typed predicate — see
    // `FilterProp::Historic` in types/ability.rs). Mirrors the "modified"
    // adjective handling above: only consume when a type word follows, so
    // bare "historic" alone doesn't hijack other contexts.
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("historic ").parse(&lower[pos..])
    {
        if starts_with_type_phrase_lead(rest) {
            properties.push(FilterProp::Historic);
            pos += lower[pos..].len() - rest.len();
        }
    }

    // Handle color prefix: "white creature", "red spell", etc.
    let color_prop = parse_color_prefix(&lower[pos..]);
    if let Some((ref prop, color_len)) = color_prop {
        properties.push(prop.clone());
        pos += color_len;
    }

    // CR 205.4b: Parse one or more comma-separated negation prefixes.
    // "noncreature, nonland permanent" → [Non(Creature), Non(Land)] in type_filters
    // "nonartifact, nonblack creature" → Non(Artifact) in type_filters, NotColor("Black") in properties
    //
    // parse_non_prefix uses whitespace as word boundary, but in stacked negation the
    // separator is ", " (comma-space). We must strip the trailing comma from the negated
    // word when the ", non" continuation pattern follows.
    let mut neg_type_filters: Vec<TypeFilter> = Vec::new();
    loop {
        let remaining = &lower[pos..];
        let Ok((after_non, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("non").parse(remaining)
        else {
            break;
        };
        // Optional hyphen: "non-" or "non"
        let after_non =
            match tag::<_, _, nom_language::error::VerboseError<&str>>("-").parse(after_non) {
                Ok((r, _)) => r,
                Err(_) => after_non,
            };
        let prefix_len = remaining.len() - after_non.len(); // "non" or "non-"

        // Find the negated word: ends at comma or whitespace
        let end = after_non
            .find(|c: char| c.is_whitespace() || c == ',')
            .unwrap_or(after_non.len());
        if end == 0 {
            break;
        }
        let negated = &after_non[..end];
        match classify_negation(negated) {
            NegationResult::Type(tf) => neg_type_filters.push(tf),
            NegationResult::Prop(prop) => properties.push(prop),
        }
        pos += prefix_len + end;

        // Check for ", non" continuation (stacked negation)
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(", ").parse(&lower[pos..])
        {
            if tag::<_, _, nom_language::error::VerboseError<&str>>("non")
                .parse(rest)
                .is_ok()
            {
                pos += ", ".len();
                continue;
            }
        }
        // Consume trailing whitespace after the negated word
        if pos < lower.len() && lower.as_bytes()[pos] == b' ' {
            pos += 1;
        }
        break;
    }

    // CR 700.6: "historic" adjective prefix can appear AFTER negation prefixes
    // (e.g. "nontoken historic permanent" in Arbaaz Mir). The pre-negation arm
    // above handles the bare-prefix case ("historic permanent"); this arm
    // handles the post-negation case so the adjective composes with `non`
    // negation. Mirrors the structural reasoning that produced
    // `is_adjective_prefix_prop` — the predicate is leg-local but its position
    // in surface text varies relative to negation.
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("historic ").parse(&lower[pos..])
    {
        if starts_with_type_phrase_lead(rest) && !properties.contains(&FilterProp::Historic) {
            properties.push(FilterProp::Historic);
            pos += lower[pos..].len() - rest.len();
        }
    }

    // Parse the core type, falling back to subtype recognition
    let (card_type, subtype, type_len) = parse_core_type(&lower[pos..]);
    pos += type_len;

    // If no core type was found, try subtype recognition as fallback.
    // "Zombies you control" → subtype="Zombie", no card_type.
    let subtype = if card_type.is_none() && subtype.is_none() {
        if let Some((sub_name, sub_len)) = parse_subtype(&lower[pos..]) {
            pos += sub_len;
            Some(sub_name)
        } else {
            None
        }
    } else {
        subtype
    };

    // CR 205.3a: "[Subtype] [CoreType]" patterns like "Wizard creatures",
    // "Goblin creatures", "Elf Warriors" — when parse_core_type (via parse_type_filter_word)
    // matched a subtype word, check if a concrete core type word follows. If so, promote
    // the subtype to the subtype slot and the trailing core type to card_type.
    // Excludes Card/Spell (handled by redundant suffix stripping) and subtypes.
    let (card_type, subtype) =
        if matches!(card_type, Some(TypeFilter::Subtype(_))) && subtype.is_none() {
            let rest_after = lower[pos..].trim_start();
            let ws = lower[pos..].len() - rest_after.len();
            if let Ok((ct_rest, tf)) = nom_target::parse_type_filter_word(rest_after) {
                let is_concrete_core_type = matches!(
                    tf,
                    TypeFilter::Creature
                        | TypeFilter::Artifact
                        | TypeFilter::Enchantment
                        | TypeFilter::Instant
                        | TypeFilter::Sorcery
                        | TypeFilter::Planeswalker
                        | TypeFilter::Land
                        | TypeFilter::Battle
                        | TypeFilter::Permanent
                );
                if is_concrete_core_type {
                    let ct_len = rest_after.len() - ct_rest.len();
                    pos += ws + ct_len;
                    let sub_name = match card_type {
                        Some(TypeFilter::Subtype(s)) => s,
                        _ => unreachable!(),
                    };
                    (Some(tf), Some(sub_name))
                } else {
                    (card_type, subtype)
                }
            } else {
                (card_type, subtype)
            }
        } else {
            (card_type, subtype)
        };

    // CR 205.2a: Multi-type adjective conjunction — "artifact creature", "legendary
    // creature", "noncreature artifact", "enchantment creature", etc. The first core
    // type was consumed above; collect trailing concrete core type words as
    // additional conjunctive type filters (evaluated via AND in `filter.rs`).
    //
    // Example: "whenever you cast an artifact creature spell" → primary = Artifact,
    // conjunctive = [Creature]. A non-creature artifact spell would NOT satisfy
    // this filter, whereas the single-type parse would have incorrectly accepted it.
    //
    // Guard: only consume adjacent core-type words (no separator between them).
    // Word-boundary on the next character prevents "creature" from eating into
    // suffixes like "creatures". Stop before `Card` / `Subtype` — those are
    // informational suffixes ("creature card") or belong to the subtype slot.
    let mut extra_core_type_filters: Vec<TypeFilter> = Vec::new();
    if matches!(
        card_type,
        Some(
            TypeFilter::Creature
                | TypeFilter::Artifact
                | TypeFilter::Enchantment
                | TypeFilter::Instant
                | TypeFilter::Sorcery
                | TypeFilter::Planeswalker
                | TypeFilter::Land
                | TypeFilter::Battle
                | TypeFilter::Permanent
        )
    ) {
        loop {
            let rest_after = lower[pos..].trim_start();
            let ws = lower[pos..].len() - rest_after.len();
            // `ws == 0` means no whitespace separator — not an adjacent adjective.
            if ws == 0 {
                break;
            }
            let Ok((ct_rest, tf)) = nom_target::parse_type_filter_word(rest_after) else {
                break;
            };
            let is_concrete_core_type = matches!(
                tf,
                TypeFilter::Creature
                    | TypeFilter::Artifact
                    | TypeFilter::Enchantment
                    | TypeFilter::Instant
                    | TypeFilter::Sorcery
                    | TypeFilter::Planeswalker
                    | TypeFilter::Land
                    | TypeFilter::Battle
            );
            if !is_concrete_core_type {
                break;
            }
            // Must not duplicate the primary or an already-accumulated filter.
            if card_type.as_ref() == Some(&tf) || extra_core_type_filters.contains(&tf) {
                break;
            }
            let ct_len = rest_after.len() - ct_rest.len();
            pos += ws + ct_len;
            extra_core_type_filters.push(tf);
        }
    }

    // Skip redundant trailing "spell"/"spells"/"card"/"cards" after a specific type like
    // "sorcery spell", "creature card". When the core type is already Instant/Sorcery/etc.,
    // the word is informational — consuming it allows suffix parsers (e.g., "that targets only")
    // and event verb parsers to see what follows.
    if card_type.is_some() && !matches!(card_type, Some(TypeFilter::Card) | Some(TypeFilter::Any)) {
        let rest_trimmed = lower[pos..].trim_start();
        let ws_len = lower[pos..].len() - rest_trimmed.len();
        // CR 108.1: "spell" and "card" are informational suffixes after a typed qualifier.
        // Longest-match-first ordering (plurals before singular).
        static REDUNDANT_SUFFIXES: &[&str] = &["spells ", "spell ", "cards ", "card "];
        let mut consumed_suffix = false;
        for suffix in REDUNDANT_SUFFIXES {
            if let Ok((after, _)) =
                tag::<_, _, nom_language::error::VerboseError<&str>>(*suffix).parse(rest_trimmed)
            {
                let suffix_len = rest_trimmed.len() - after.len();
                pos += ws_len + suffix_len;
                consumed_suffix = true;
                break;
            }
        }
        if !consumed_suffix {
            // Check end-of-input variants (no trailing space)
            for suffix in &["spells", "spell", "cards", "card"] {
                if rest_trimmed == *suffix {
                    pos += ws_len + suffix.len();
                    break;
                }
            }
        }
    }

    if let Some(consumed) = parse_token_suffix(&lower[pos..]) {
        properties.push(FilterProp::Token);
        pos += consumed;
    }

    // CR 205.3a: Comma-separated type lists ("artifacts, creatures, and lands") are
    // syntactic sugar for set-union, same as "and" between two types.
    let rest_lower = lower[pos..].trim_start();
    let rest_offset = lower[pos..].len() - rest_lower.len();

    // Try each type combinator separator in longest-match-first order.
    // Each separator produces an Or combination when followed by a recognized type word.
    static TYPE_SEPARATORS: &[&str] = &[
        ", and/or ",
        ", and ",
        ", or ",
        ", ",
        "or ",
        "and/or ",
        "and ",
    ];
    for separator in TYPE_SEPARATORS {
        if let Ok((after_sep, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(*separator).parse(rest_lower)
        {
            let after_trimmed = after_sep.trim_start();
            if starts_with_type_word(after_trimmed) {
                let sep_text = &text[pos + rest_offset + separator.len()..];
                let (other_filter, final_rest) = parse_type_phrase(sep_text);
                let left = typed(
                    card_type.unwrap_or(TypeFilter::Any),
                    subtype,
                    properties.clone(),
                    neg_type_filters.clone(),
                );
                let combined = merge_or_filters(left, other_filter);
                let combined = distribute_shared_properties(combined, &properties);
                let combined = distribute_controller_to_or(combined);
                return (distribute_properties_to_or(combined), final_rest);
            }
        }
    }

    // CR 108.3 + CR 110.2: Ownership and control are distinct; "you own and control" satisfies both.
    let mut controller = None;
    pos += parse_ownership_or_controller_suffix(&lower[pos..], &mut properties, &mut controller);

    // Check "with power N or less/greater" suffix
    if let Some((prop, consumed)) = parse_mana_value_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    // Check "with power N or less/greater" suffix
    if let Some((prop, consumed)) = parse_power_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    // Check "with [counter] counter(s) on it/them" suffix
    if let Some((prop, consumed)) = parse_counter_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    if let Some((keyword_props, consumed)) = parse_without_keyword_suffix(&lower[pos..]) {
        properties.extend(keyword_props);
        pos += consumed;
    } else if let Some((keyword_props, consumed)) = parse_keyword_suffix(&lower[pos..]) {
        properties.extend(keyword_props);
        pos += consumed;
    }

    if controller.is_none()
        && !properties
            .iter()
            .any(|prop| matches!(prop, FilterProp::Owned { .. }))
    {
        pos +=
            parse_ownership_or_controller_suffix(&lower[pos..], &mut properties, &mut controller);
    }

    // CR 205.3 + CR 205.4b: "that isn't a <Subtype>" relative-clause negation.
    // Checked before `parse_that_clause_suffix` so the subtype exclusion short-circuits
    // the generic that-clause branch (which does not recognize subtype negation).
    if let Some((neg_tfs, consumed)) = parse_that_isnt_subtype_suffix(&lower[pos..]) {
        neg_type_filters.extend(neg_tfs);
        pos += consumed;
    }

    // CR 700.5: "that share(s) a creature type" / "that has/have [keyword]" relative clause.
    if let Some((that_props, consumed)) = parse_that_clause_suffix(&lower[pos..]) {
        properties.extend(that_props);
        pos += consumed;
    }

    // Check zone suffix: "card from a graveyard", "card in your graveyard", "from exile", etc.
    if let Some((zone_props, zone_ctrl, consumed)) = parse_zone_suffix(&lower[pos..]) {
        properties.extend(zone_props);
        pos += consumed;
        // Apply zone-derived controller if we don't already have one
        if controller.is_none() {
            controller = zone_ctrl;
        }
    }

    // Check "of the chosen type" suffix (Cavern of Souls, Metallic Mimic, etc.)
    let remaining = lower[pos..].trim_start();
    let remaining_offset = lower[pos..].len() - remaining.len();
    if tag::<_, _, nom_language::error::VerboseError<&str>>("of the chosen type")
        .parse(remaining)
        .is_ok()
    {
        properties.push(FilterProp::IsChosenCreatureType);
        pos += remaining_offset + "of the chosen type".len();
    }

    let mut exclude_chosen_type = false;
    let remaining = lower[pos..].trim_start();
    let remaining_offset = lower[pos..].len() - remaining.len();
    for suffix in &[
        "that aren't of the chosen type",
        "that are not of the chosen type",
        "not of the chosen type",
    ] {
        if tag::<_, _, nom_language::error::VerboseError<&str>>(*suffix)
            .parse(remaining)
            .is_ok()
        {
            exclude_chosen_type = true;
            pos += remaining_offset + suffix.len();
            break;
        }
    }

    // CR 406.6: "exiled with [source]" linkage suffix on a typed reference.
    // Singular targeted form ("target creature card exiled with ~") composes
    // with the typed filter via `TargetFilter::And { [Typed, ExiledBySource] }`,
    // mirroring the `exclude_chosen_type` wrapping pattern below. The plural
    // and "each card" forms are handled at the top of `parse_target` since
    // they bypass type-phrase parsing entirely.
    let mut exiled_by_source = false;
    let remaining_exiled = lower[pos..].trim_start();
    let exiled_offset = lower[pos..].len() - remaining_exiled.len();
    // Try "exiled with this <type>" first (longest-match-first); the trailing
    // type word identifies the source object's card type and is informational
    // here — consume it as a single non-space run via take_till1 so it doesn't
    // leak into the remainder.
    if let Ok((rest, _)) = (
        tag::<_, _, nom_language::error::VerboseError<&str>>("exiled with this "),
        nom::bytes::complete::take_till1::<_, _, nom_language::error::VerboseError<&str>>(
            |c: char| c.is_whitespace(),
        ),
    )
        .parse(remaining_exiled)
    {
        exiled_by_source = true;
        pos += exiled_offset + (remaining_exiled.len() - rest.len());
    } else if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("exiled with ~")
            .parse(remaining_exiled)
    {
        exiled_by_source = true;
        pos += exiled_offset + (remaining_exiled.len() - rest.len());
    }

    // CR 608.2d: "of their choice" / "of his or her choice" — informational qualifier
    // on opponent-choice effects. The actual choice is handled by the WaitingFor state machine.
    let remaining_choice = lower[pos..].trim_start();
    let choice_offset = lower[pos..].len() - remaining_choice.len();
    for suffix in &["of their choice", "of his or her choice"] {
        if tag::<_, _, nom_language::error::VerboseError<&str>>(*suffix)
            .parse(remaining_choice)
            .is_ok()
        {
            pos += choice_offset + suffix.len();
            break;
        }
    }

    // CR 201.2: "named [card name]" suffix — filter by exact card name.
    // Handles "creature named X", "cards named X", "named X" patterns.
    let remaining_named = lower[pos..].trim_start();
    let named_offset = lower[pos..].len() - remaining_named.len();
    if let Ok((name_text, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("named ").parse(remaining_named)
    {
        // Name extends to end-of-clause markers: comma, period, "you control", "that", or end.
        let name_end = name_text.find([',', '.']).unwrap_or(name_text.len());
        let raw_name = name_text[..name_end].trim();
        if !raw_name.is_empty() {
            // Reconstruct original-case name from the same position in `text`
            let orig_offset = pos + named_offset + "named ".len();
            let orig_name = text[orig_offset..orig_offset + raw_name.len()].trim();
            properties.push(FilterProp::Named {
                name: orig_name.to_string(),
            });
            pos += named_offset + "named ".len() + name_end;
        }
    }

    let filter = TargetFilter::Typed(TypedFilter {
        type_filters: [
            card_type.map(|ct| vec![ct]).unwrap_or_default(),
            extra_core_type_filters,
            subtype
                .map(|s| vec![TypeFilter::Subtype(s)])
                .unwrap_or_default(),
            neg_type_filters,
        ]
        .concat(),
        controller,
        properties,
    });
    let filter = if exclude_chosen_type {
        TargetFilter::And {
            filters: vec![
                filter,
                TargetFilter::Not {
                    filter: Box::new(TargetFilter::Typed(
                        TypedFilter::default().properties(vec![FilterProp::IsChosenCreatureType]),
                    )),
                },
            ],
        }
    } else {
        filter
    };

    // CR 406.6: Compose the typed filter with the exile-link constraint when
    // the singular "exiled with ~" suffix was present. Runtime evaluation of
    // `TargetFilter::And` requires every inner filter to match (game/filter.rs
    // line 347), and `extract_in_zone` surfaces `Zone::Exile` from the
    // `ExiledBySource` arm so the resolver scans the correct zone.
    let filter = if exiled_by_source {
        TargetFilter::And {
            filters: vec![filter, TargetFilter::ExiledBySource],
        }
    } else {
        filter
    };

    (filter, &text[pos..])
}

/// Result of classifying a negated word — routes to `type_filters` or `properties`.
enum NegationResult {
    /// Core type/subtype negation → goes into `type_filters`
    Type(TypeFilter),
    /// Color/supertype negation → stays in `properties`
    Prop(FilterProp),
}

/// CR 205.4b: Classify a negated word by semantic layer.
/// `parse_non_prefix` strips "non"/"non-" and lowercases, so `negated` is e.g. "black", "basic", "creature".
fn classify_negation(negated: &str) -> NegationResult {
    match negated {
        // Color negation — parallel to HasColor
        "white" => NegationResult::Prop(FilterProp::NotColor {
            color: ManaColor::White,
        }),
        "blue" => NegationResult::Prop(FilterProp::NotColor {
            color: ManaColor::Blue,
        }),
        "black" => NegationResult::Prop(FilterProp::NotColor {
            color: ManaColor::Black,
        }),
        "red" => NegationResult::Prop(FilterProp::NotColor {
            color: ManaColor::Red,
        }),
        "green" => NegationResult::Prop(FilterProp::NotColor {
            color: ManaColor::Green,
        }),
        // CR 205.4a: Supertype negation — parallel to HasSupertype
        "basic" => NegationResult::Prop(FilterProp::NotSupertype {
            value: Supertype::Basic,
        }),
        "legendary" => NegationResult::Prop(FilterProp::NotSupertype {
            value: Supertype::Legendary,
        }),
        "snow" => NegationResult::Prop(FilterProp::NotSupertype {
            value: Supertype::Snow,
        }),
        // CR 205.4b: Type/subtype negation → TypeFilter::Non
        _ => {
            let inner = match negated {
                "creature" => TypeFilter::Creature,
                "land" => TypeFilter::Land,
                "artifact" => TypeFilter::Artifact,
                "enchantment" => TypeFilter::Enchantment,
                "instant" => TypeFilter::Instant,
                "sorcery" => TypeFilter::Sorcery,
                "planeswalker" => TypeFilter::Planeswalker,
                other => TypeFilter::Subtype(capitalize_first(other)),
            };
            NegationResult::Type(TypeFilter::Non(Box::new(inner)))
        }
    }
}

/// Guard: does text start with something `parse_type_phrase` would recognize?
/// Used to prevent comma/and/or recursion on non-type text.
pub(crate) fn starts_with_type_word(text: &str) -> bool {
    // Core type: "creature", "artifact", "permanent", etc.
    if parse_core_type(text).0.is_some() {
        return true;
    }
    // Subtype: "zombie", "vampires", "elves", etc.
    if parse_subtype(text).is_some() {
        return true;
    }
    // Standalone "token"/"tokens" (property word, not a core type or subtype).
    // Reuses parse_token_suffix which handles singular/plural with word boundary.
    if parse_token_suffix(text).is_some() {
        return true;
    }
    // CR 105.1: Color adjective prefix: "blue creature", "red permanent", etc.
    // parse_type_phrase handles color prefixes internally, but the article guard
    // must recognize them to strip "a "/"an " correctly.
    if let Ok((rest, _)) = nom_primitives::parse_color(text) {
        if let Ok((after_space, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(" ").parse(rest)
        {
            if starts_with_type_word(after_space) {
                return true;
            }
        }
    }
    // CR 205.4b: Negated type prefix: "noncreature spell", "nonland permanent"
    if let Ok((after_non, _)) = alt((
        tag::<_, _, nom_language::error::VerboseError<&str>>("non-"),
        tag("non"),
    ))
    .parse(text)
    {
        // Consume the negated word up to whitespace, then check for a core type after.
        if let Ok((after_space, _)) = (
            take_till::<_, _, nom_language::error::VerboseError<&str>>(|c: char| c.is_whitespace()),
            tag::<_, _, nom_language::error::VerboseError<&str>>(" "),
        )
            .parse(after_non)
        {
            if parse_core_type(after_space).0.is_some() {
                return true;
            }
        }
    }
    // CR 700.4 + CR 700.9: "modified <type>" adjective phrase leads a type
    // phrase (e.g., "modified creatures you control"). Consume the adjective
    // and verify a type word follows so the comma/and-list recursion can
    // continue across the "modified" leg.
    if let Ok((after_modified, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("modified ").parse(text)
    {
        if starts_with_type_phrase_lead(after_modified) {
            return true;
        }
    }
    // CR 700.6: "historic <type>" adjective phrase leads a type phrase
    // (e.g., "historic permanents you control"). Consume the adjective and
    // verify a type word follows so the comma/and-list recursion can continue
    // across the "historic" leg.
    if let Ok((after_historic, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("historic ").parse(text)
    {
        if starts_with_type_phrase_lead(after_historic) {
            return true;
        }
    }
    false
}

fn starts_with_type_phrase_lead(text: &str) -> bool {
    let text = text.trim_start();
    starts_with_type_word(text)
        || nom_target::parse_supertype_prefix(text).is_ok()
        || parse_color_prefix(text).is_some()
}

fn target_filter_has_meaningful_content(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => !tf.type_filters.is_empty() || !tf.properties.is_empty(),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_has_meaningful_content)
        }
        _ => false,
    }
}

fn distribute_shared_properties(filter: TargetFilter, shared_props: &[FilterProp]) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            for prop in shared_props {
                if !typed
                    .properties
                    .iter()
                    .any(|existing| prop.same_kind(existing))
                {
                    typed.properties.push(prop.clone());
                }
            }
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| distribute_shared_properties(filter, shared_props))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|filter| distribute_shared_properties(filter, shared_props))
                .collect(),
        },
        other => other,
    }
}

/// Returns true when the given property is leg-local (produced by an adjective
/// prefix during `parse_type_phrase` scanning) and must NOT distribute back
/// across earlier legs of a comma-OR list. Every other property is assumed to
/// originate from a trailing-suffix parser and is eligible for distribution —
/// e.g., "artifacts and creatures with mana value 2 or less" distributes
/// `CmcLE` back onto the artifact leg, while "Auras, Equipment, and modified
/// creatures you control" must NOT propagate `FilterProp::Modified` to the
/// Aura/Equipment legs.
fn is_adjective_prefix_prop(prop: &FilterProp) -> bool {
    matches!(
        prop,
        // CR 700.4 + CR 700.9: "modified [type]" adjective prefix.
        FilterProp::Modified
            // CR 700.6: "historic [type]" adjective prefix.
            | FilterProp::Historic
            // CR 303.4 + CR 301.5: "enchanted [type]" / "equipped [type]".
            | FilterProp::EnchantedBy
            | FilterProp::EquippedBy
            // CR 115.10a: "another [type]" / "other [type]".
            | FilterProp::Another
            // CR 110.5: "tapped [type]" / "untapped [type]".
            | FilterProp::Tapped
            | FilterProp::Untapped
            // CR 509.1h: combat-status prefixes "attacking/blocking/unblocked".
            | FilterProp::Attacking
            | FilterProp::Blocking
            | FilterProp::Unblocked
            // CR 105.1 + CR 205.2: color / supertype adjectives.
            | FilterProp::HasColor { .. }
            | FilterProp::NotColor { .. }
            | FilterProp::HasSupertype { .. }
            | FilterProp::NotSupertype { .. }
            // Token qualifier ("creature tokens").
            | FilterProp::Token
    )
}

/// Distribute trailing filter properties (CmcLE, CmcGE, PowerLE, PowerGE, etc.)
/// from the last `Typed` element in an `Or` filter to all preceding `Typed`
/// elements that lack a property of the same kind.
/// Handles "artifacts and creatures with mana value 2 or less" where only the
/// final type parses the "with mana value N or less/greater" suffix.
///
/// CR 700.4: Only distributes props produced by trailing-suffix parsers. Props
/// produced by adjective prefixes (e.g. FilterProp::Modified from "modified
/// creatures", FilterProp::EnchantedBy from "enchanted creature") are
/// leg-local and retained only on their originating leg. See
/// `is_trailing_suffix_prop`.
fn distribute_properties_to_or(filter: TargetFilter) -> TargetFilter {
    let TargetFilter::Or { mut filters } = filter else {
        return filter;
    };

    // Collect trailing-suffix properties from the last Typed element. Filter
    // out adjective-prefix props (CR 700.4, etc.) that are leg-local.
    let trailing_props: Vec<FilterProp> = filters
        .iter()
        .rev()
        .find_map(|f| {
            if let TargetFilter::Typed(TypedFilter { properties, .. }) = f {
                let suffix_props: Vec<FilterProp> = properties
                    .iter()
                    .filter(|p| !is_adjective_prefix_prop(p))
                    .cloned()
                    .collect();
                if suffix_props.is_empty() {
                    None
                } else {
                    Some(suffix_props)
                }
            } else {
                None
            }
        })
        .unwrap_or_default();

    if !trailing_props.is_empty() {
        for f in &mut filters {
            if let TargetFilter::Typed(ref mut typed) = f {
                for prop in &trailing_props {
                    if !typed.properties.iter().any(|p| prop.same_kind(p)) {
                        typed.properties.push(prop.clone());
                    }
                }
            }
        }
    }

    TargetFilter::Or { filters }
}

/// Distribute the controller from the last `Typed` element in an `Or` filter
/// to all preceding `Typed` elements that have `controller: None`.
/// Handles "artifacts, creatures, and lands your opponents control" where only
/// the final type parses the controller suffix.
fn distribute_controller_to_or(filter: TargetFilter) -> TargetFilter {
    let TargetFilter::Or { mut filters } = filter else {
        return filter;
    };

    // Find the controller from the last Typed element (reverse search)
    let controller = filters.iter().rev().find_map(|f| {
        if let TargetFilter::Typed(TypedFilter {
            controller: Some(ref ctrl),
            ..
        }) = f
        {
            Some(ctrl.clone())
        } else {
            None
        }
    });

    if let Some(ctrl) = controller {
        for f in &mut filters {
            if let TargetFilter::Typed(ref mut typed) = f {
                if typed.controller.is_none() {
                    typed.controller = Some(ctrl.clone());
                }
            }
        }
    }

    TargetFilter::Or { filters }
}

fn parse_core_type(text: &str) -> (Option<TypeFilter>, Option<String>, usize) {
    // Delegate to the shared nom combinator table which handles both singular
    // and plural forms in longest-match-first order.
    if let Ok((rest, tf)) = nom_target::parse_type_filter_word(text) {
        let consumed = text.len() - rest.len();
        return (Some(tf), None, consumed);
    }

    (None, None, 0)
}

/// Parse a controller suffix like " you control", " an opponent controls", " your opponents control".
/// Returns `(ControllerRef, bytes_consumed)` where consumed includes leading whitespace.
///
/// Delegates to `nom_target::parse_controller_suffix` for the common patterns
/// ("you control", "an opponent controls", "your opponents control"), then
/// handles additional patterns not in the shared combinator.
fn parse_controller_suffix(text: &str) -> Option<(ControllerRef, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();

    // Delegate to nom_filter::parse_zone_controller which handles common patterns,
    // then fall through to additional nom-based patterns.
    if let Ok((rest, ctrl)) = nom_filter::parse_zone_controller(trimmed) {
        return Some((ctrl, leading_ws + trimmed.len() - rest.len()));
    }

    // Additional patterns via nom tag().
    // Note: "target player controls" is handled by `parse_zone_controller` above
    // (single-authority for `ControllerRef::TargetPlayer`).
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("that player controls").parse(trimmed)
    {
        // CR 109.4 + CR 115.1: "that player controls" is a relative reference
        // back to a player introduced earlier in the ability (e.g. the attacked
        // player in a "whenever you attack a player, ... that player controls"
        // trigger). When the surrounding parser pushed a relative-player scope
        // (see `oracle_target_scope`), emit `ControllerRef::TargetPlayer` so the
        // runtime auto-surfaces a companion `TargetFilter::Player` slot via
        // `effect_references_target_player` (game/ability_utils.rs). Without a
        // scope, fall back to the legacy `ControllerRef::You` behaviour relied
        // on by per-player iteration contexts (`resolve_quantity_scoped`).
        let ctrl = oracle_target_scope::current().unwrap_or(ControllerRef::You);
        return Some((ctrl, leading_ws + trimmed.len() - rest.len()));
    }
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("they control").parse(trimmed)
    {
        // CR 608.2d: "they control" → ControllerRef::You, resolved against
        // accepting_player during "any opponent may" resolution.
        return Some((ControllerRef::You, leading_ws + trimmed.len() - rest.len()));
    }

    None
}

fn parse_token_suffix(text: &str) -> Option<usize> {
    let trimmed = text.trim_start();

    // Try "tokens" before "token" (longest match first), with word boundary.
    for word in &["tokens", "token"] {
        if let Ok((rest, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(*word).parse(trimmed)
        {
            match rest.chars().next() {
                None | Some(' ' | ',' | '.') => return Some(text.len() - rest.len()),
                _ => {}
            }
        }
    }

    None
}

/// Parse a color adjective prefix: "white ", "blue ", "black ", "red ", "green ".
/// Returns (FilterProp::HasColor, bytes consumed including trailing space).
///
/// Delegates to `nom_primitives::parse_color` for color word recognition,
/// then verifies a trailing space exists (color as adjective, not standalone).
fn parse_color_prefix(text: &str) -> Option<(FilterProp, usize)> {
    let (rest, color) = nom_primitives::parse_color(text).ok()?;
    // Must be followed by a space (color adjective prefix, not standalone color word).
    let (rest, _) = tag::<_, _, nom_language::error::VerboseError<&str>>(" ")
        .parse(rest)
        .ok()?;
    let consumed = text.len() - rest.len();
    Some((FilterProp::HasColor { color }, consumed))
}

/// CR 509.1h / CR 302.6: Parse status prefixes from type phrases.
/// Called in a loop to consume multiple prefixes (e.g. "unblocked attacking ").
/// Handles combat status (attacking, unblocked) and tap status (tapped, untapped).
///
/// Delegates to `nom_filter::parse_property_filter` for the common property keywords,
/// then handles "face-down " (hyphenated variant not in the nom combinator).
pub(crate) fn parse_combat_status_prefix(text: &str) -> Option<(FilterProp, usize)> {
    // Try the shared nom property filter combinator for combat/tap status keywords.
    // Filter to only the status properties relevant as type phrase prefixes.
    if let Ok((rest, prop)) = nom_filter::parse_property_filter(text) {
        if matches!(
            prop,
            FilterProp::Unblocked
                | FilterProp::Attacking
                | FilterProp::Blocking
                | FilterProp::Tapped
                | FilterProp::Untapped
                | FilterProp::FaceDown
        ) {
            // Must be followed by space (prefix, not standalone)
            if let Ok((after_space, _)) =
                tag::<_, _, nom_language::error::VerboseError<&str>>(" ").parse(rest)
            {
                return Some((prop, text.len() - after_space.len()));
            }
        }
    }

    // Handle "face-down " (hyphenated variant not in the nom combinator).
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("face-down ").parse(text)
    {
        return Some((FilterProp::FaceDown, text.len() - rest.len()));
    }

    None
}

/// Parse "with power [or toughness] N or less/greater", "with toughness N or
/// less/greater", and "with greater power" suffixes. Returns `(FilterProp,
/// bytes consumed from the original text)`. CR 208.1 governs P/T comparisons;
/// CR 509.1b covers the source-relative "greater power" form.
///
/// The disjunctive form "with power or toughness N or less" produces
/// `FilterProp::AnyOf { [PowerLE(N), ToughnessLE(N)] }` so the composite
/// matches when *either* property is at or below N — matching the natural-
/// language semantics of "power or toughness". Used by Arnyn Deathbloom
/// Botanist, Stern Scolding, Leonardo Sewer Samurai, Warping Wail, etc.
fn parse_power_suffix(text: &str) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();

    // CR 509.1b: "with greater power" — relative to the source object.
    if let Ok((after, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("with greater power").parse(trimmed)
    {
        return Some((FilterProp::PowerGTSource, text.len() - after.len()));
    }

    // Longest-match first: disjunctive "with power or toughness N or {less,greater}".
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("with power or toughness ")
            .parse(trimmed)
    {
        let (rest, value) = nom_quantity::parse_quantity_expr_number(rest).ok()?;
        let after_num = rest.trim_start();
        if let Ok((after, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("or less").parse(after_num)
        {
            let props = vec![
                FilterProp::PowerLE {
                    value: value.clone(),
                },
                FilterProp::ToughnessLE { value },
            ];
            return Some((FilterProp::AnyOf { props }, text.len() - after.len()));
        }
        if let Ok((after, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("or greater").parse(after_num)
        {
            let props = vec![
                FilterProp::PowerGE {
                    value: value.clone(),
                },
                FilterProp::ToughnessGE { value },
            ];
            return Some((FilterProp::AnyOf { props }, text.len() - after.len()));
        }
        return None;
    }

    // "with toughness N or less/greater" — CR 208.1, mirrors the power form.
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("with toughness ").parse(trimmed)
    {
        let (rest, value) = nom_quantity::parse_quantity_expr_number(rest).ok()?;
        let after_num = rest.trim_start();
        let (prop, after) = if let Ok((a, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("or less").parse(after_num)
        {
            (FilterProp::ToughnessLE { value }, a)
        } else if let Ok((a, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("or greater").parse(after_num)
        {
            (FilterProp::ToughnessGE { value }, a)
        } else {
            return None;
        };
        return Some((prop, text.len() - after.len()));
    }

    let (rest, _) = tag::<_, _, nom_language::error::VerboseError<&str>>("with power ")
        .parse(trimmed)
        .ok()?;
    // CR 208.1 + CR 107.3a: Accept literal N or the variable X — X emits
    // `QuantityRef::Variable { "X" }` resolved at effect time against the
    // resolving ability's `chosen_x` via `FilterContext::from_ability`.
    let (rest, value) = nom_quantity::parse_quantity_expr_number(rest).ok()?;
    let after_num = rest.trim_start();
    let (prop, after) = if let Ok((a, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("or less").parse(after_num)
    {
        (FilterProp::PowerLE { value }, a)
    } else if let Ok((a, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("or greater").parse(after_num)
    {
        (FilterProp::PowerGE { value }, a)
    } else {
        return None;
    };
    Some((prop, text.len() - after.len()))
}

/// Parse "with mana value N or less" / "with mana value N or greater" suffix,
/// and dynamic "with mana value less than or equal to that [type]" patterns.
/// Returns (FilterProp, bytes consumed from the original text).
pub(crate) fn parse_mana_value_suffix(text: &str) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();
    let (rest, _) = tag::<_, _, nom_language::error::VerboseError<&str>>("with mana value ")
        .parse(trimmed)
        .ok()?;

    // CR 202.3 + CR 120.3: Dynamic comparisons referencing the triggering event.
    // "that damage" → `EventContextAmount` (damage amount captured at trigger).
    // "that <type>" (e.g. "that creature", "that spell") → `EventContextSourceManaValue`
    // (mana value of the triggering source object).
    // Staged checks: first detect "less than" / "greater than", then check for "or equal to".
    type Vbe<'a> = nom_language::error::VerboseError<&'a str>;
    let try_dynamic = |rest: &str, is_le: bool| -> Option<(FilterProp, usize)> {
        let kw_tag = if is_le { "less than" } else { "greater than" };
        let (a, _) = tag::<_, _, Vbe>(kw_tag).parse(rest).ok()?;
        let a = a.trim_start();
        let (is_equal, a) = if let Ok((a2, _)) = tag::<_, _, Vbe>("or equal to").parse(a) {
            (true, a2.trim_start())
        } else {
            (false, a)
        };
        // CR 120.3: Anaphoric "that <noun>" forms — bind to the trigger context.
        // CR 119.3: Non-anaphoric quantity-ref forms — bind to a static or
        // game-state quantity ("the number of lands you control",
        // "the number of cards in your graveyard", "the amount of life you
        // gained this turn", etc.). The two forms are mutually exclusive at
        // this position; try anaphoric first, then fall through.
        let (qty, after) = if let Ok((a2, _)) = tag::<_, _, Vbe>("that ").parse(a) {
            // CR 120.3: "that damage" — the damage amount captured by the trigger
            // (DamageDone events stamp `EventContextAmount`).
            if let Ok((a3, _)) = tag::<_, _, Vbe>("damage").parse(a2) {
                (QuantityRef::EventContextAmount, a3)
            } else {
                // Fall back to the type-word arm — "that <type>" where <type> is any
                // single word terminating at punctuation/space (e.g., "creature",
                // "spell"). Uses the source object's mana value.
                let after = a2.find([',', '.', ' ']).map_or(a2, |i| &a2[i..]);
                (QuantityRef::EventContextSourceManaValue, after)
            }
        } else {
            // CR 119.3: Generic quantity-ref RHS — extract the phrase up to the
            // next sentence-terminating punctuation and delegate to the shared
            // `parse_quantity_ref` building block. Unlocks Vhal's "the number
            // of study counters removed this way", Beseech the Queen's "the
            // number of lands you control", Bring to Light's "the number of
            // colors of mana spent to cast this spell", etc. The terminator
            // boundary (comma / period / end-of-input) prevents over-consuming
            // into trailing search-and-shuffle clauses ("…, reveal it, put it
            // into your hand" on Beseech the Queen).
            let phrase_end = a.find([',', '.']).unwrap_or(a.len());
            let phrase = &a[..phrase_end];
            let qty = crate::parser::oracle_quantity::parse_quantity_ref(phrase)?;
            (qty, &a[phrase_end..])
        };
        let make_value = |off: i32| {
            if off == 0 {
                QuantityExpr::Ref { qty }
            } else {
                QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref { qty }),
                    offset: off,
                }
            }
        };
        let prop = match (is_le, is_equal) {
            (true, true) => FilterProp::CmcLE {
                value: make_value(0),
            },
            (true, false) => FilterProp::CmcLE {
                value: make_value(-1),
            },
            (false, true) => FilterProp::CmcGE {
                value: make_value(0),
            },
            (false, false) => FilterProp::CmcGE {
                value: make_value(1),
            },
        };
        Some((prop, text.len() - after.len()))
    };
    if let Some(found) = try_dynamic(rest, true) {
        return Some(found);
    }
    if let Some(found) = try_dynamic(rest, false) {
        return Some(found);
    }

    // Static "N or less" / "N or greater" — also accepts literal X via
    // `parse_quantity_expr_number`, which emits `QuantityRef::Variable { "X" }`
    // resolved at effect time against the resolving ability's `chosen_x`.
    // CR 107.3a + CR 601.2b: X announced at cast, read at resolution.
    let (after_num_raw, value) = nom_quantity::parse_quantity_expr_number(rest).ok()?;
    let after_num = after_num_raw.trim_start();

    let (prop, after) = if let Ok((a, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("or greater").parse(after_num)
    {
        (FilterProp::CmcGE { value }, a)
    } else if let Ok((a, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("or less").parse(after_num)
    {
        (FilterProp::CmcLE { value }, a)
    } else {
        // CR 202.3: Exact mana value match — "with mana value N" (no "or less"/"or greater").
        (FilterProp::CmcEQ { value }, after_num)
    };
    Some((prop, text.len() - after.len()))
}

/// Parse "with [count] [counter] counter(s) on it/them" using pure nom combinators.
/// Returns (FilterProp, bytes consumed from the original text).
///
/// Grammar: `"with " <count> <counter_type> (" counter" | " counters") " on " ("it" | "them")`
/// where `<count>` is either an article ("a"/"an", implying 1) or a quantity
/// expression (literal N or variable X). Supports both "with a stun counter on it"
/// (fixed 1) and "with X +1/+1 counters on it" (dynamic — resolves against
/// `chosen_x` via `FilterContext::from_ability`).
///
/// CR 107.3a + CR 601.2b: X counts resolve at effect time against
/// `ResolvedAbility::chosen_x` via `FilterContext::from_ability`.
pub(crate) fn parse_counter_suffix(text: &str) -> Option<(FilterProp, usize)> {
    use nom::branch::alt;
    use nom::bytes::complete::{tag as tag_e, take_until};
    use nom::combinator::{opt, value};

    let trimmed = text.trim_start();
    let (rest, _) = tag_e::<_, _, nom_language::error::VerboseError<&str>>("with ")
        .parse(trimmed)
        .ok()?;

    // Parse count: optional article ("a"/"an" → implicit 1) or an explicit
    // quantity expression followed by a space. Neither branch matching means
    // the counter type follows "with " directly (e.g. "with ice counters on them"),
    // which is implicit count 1.
    let count_parser = alt((
        value(
            QuantityExpr::Fixed { value: 1 },
            alt((tag_e("an "), tag_e("a "))),
        ),
        |input| {
            let (input, expr) = nom_quantity::parse_quantity_expr_number(input)?;
            let (input, _) =
                tag_e::<_, _, nom_language::error::VerboseError<&str>>(" ").parse(input)?;
            Ok((input, expr))
        },
    ));
    let (rest, count_opt) = opt(count_parser).parse(rest).ok()?;
    let count = count_opt.unwrap_or(QuantityExpr::Fixed { value: 1 });

    // Try each counter suffix; pick the first that matches via `take_until`.
    // `take_until` is pure nom — the counter-type text is everything before the
    // first occurrence of the target suffix.
    for suffix in [
        " counters on them",
        " counters on it",
        " counter on them",
        " counter on it",
    ] {
        let Ok((after, counter_text)) =
            take_until::<_, _, nom_language::error::VerboseError<&str>>(suffix).parse(rest)
        else {
            continue;
        };
        let counter_type = counter_text.trim();
        if counter_type.is_empty() {
            continue;
        }
        let consumed = text.len() - after.len() + suffix.len();
        return Some((
            FilterProp::CountersGE {
                counter_type: crate::types::counter::parse_counter_type(counter_type),
                count,
            },
            consumed,
        ));
    }

    None
}

fn parse_keyword_suffix(text: &str) -> Option<(Vec<FilterProp>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    let (after_with, _) = tag::<_, _, nom_language::error::VerboseError<&str>>("with ")
        .parse(trimmed)
        .ok()?;
    let mut remaining = after_with;
    let mut consumed = leading_ws + "with ".len();
    let mut properties = Vec::new();

    while let Some((keyword_match, keyword_len)) = parse_leading_keyword_match(remaining) {
        match keyword_match {
            KeywordMatch::Concrete(keyword) => {
                properties.push(FilterProp::WithKeyword { value: keyword });
            }
            KeywordMatch::Kind(kind) => {
                properties.push(FilterProp::HasKeywordKind { value: kind });
            }
        }
        consumed += keyword_len;
        remaining = &remaining[keyword_len..];

        // Try keyword list separators in longest-match-first order.
        let mut found_sep = false;
        for sep in &[", and ", " and ", ", "] {
            if let Ok((rest, _)) =
                tag::<_, _, nom_language::error::VerboseError<&str>>(*sep).parse(remaining)
            {
                consumed += sep.len();
                remaining = rest;
                found_sep = true;
                break;
            }
        }
        if !found_sep {
            break;
        }
    }

    if properties.is_empty() {
        None
    } else {
        Some((properties, consumed))
    }
}

/// Parse "without [keyword]" suffix — negated keyword filter.
/// Handles "without flying", "without first strike", etc.
/// Parallels `parse_keyword_suffix` but emits `WithoutKeyword`.
fn parse_without_keyword_suffix(text: &str) -> Option<(Vec<FilterProp>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    let (after_without, _) = tag::<_, _, nom_language::error::VerboseError<&str>>("without ")
        .parse(trimmed)
        .ok()?;
    let mut remaining = after_without;
    let mut consumed = leading_ws + "without ".len();
    let mut properties = Vec::new();

    while let Some((keyword_match, keyword_len)) = parse_leading_keyword_match(remaining) {
        match keyword_match {
            KeywordMatch::Concrete(keyword) => {
                properties.push(FilterProp::WithoutKeyword { value: keyword });
            }
            KeywordMatch::Kind(kind) => {
                properties.push(FilterProp::WithoutKeywordKind { value: kind });
            }
        }
        consumed += keyword_len;
        remaining = &remaining[keyword_len..];

        // Try keyword list separators in longest-match-first order.
        let mut found_sep = false;
        for sep in &[", and ", " and ", ", "] {
            if let Ok((rest, _)) =
                tag::<_, _, nom_language::error::VerboseError<&str>>(*sep).parse(remaining)
            {
                consumed += sep.len();
                remaining = rest;
                found_sep = true;
                break;
            }
        }
        if !found_sep {
            break;
        }
    }

    if properties.is_empty() {
        None
    } else {
        Some((properties, consumed))
    }
}

fn parse_ownership_or_controller_suffix(
    text: &str,
    properties: &mut Vec<FilterProp>,
    controller: &mut Option<ControllerRef>,
) -> usize {
    let own_ctrl = text.trim_start();
    let own_ctrl_offset = text.len() - own_ctrl.len();
    if tag::<_, _, nom_language::error::VerboseError<&str>>("you own and control")
        .parse(own_ctrl)
        .is_ok()
    {
        *controller = Some(ControllerRef::You);
        properties.push(FilterProp::Owned {
            controller: ControllerRef::You,
        });
        return own_ctrl_offset + "you own and control".len();
    }
    if tag::<_, _, nom_language::error::VerboseError<&str>>("you own")
        .parse(own_ctrl)
        .is_ok()
        && tag::<_, _, nom_language::error::VerboseError<&str>>("you own and")
            .parse(own_ctrl)
            .is_err()
    {
        properties.push(FilterProp::Owned {
            controller: ControllerRef::You,
        });
        return own_ctrl_offset + "you own".len();
    }
    // CR 108.3: "an opponent owns" — the card belongs to an opponent, used by Eldrazi Processors.
    for phrase in ["an opponent owns", "opponents own"] {
        if tag::<_, _, nom_language::error::VerboseError<&str>>(phrase)
            .parse(own_ctrl)
            .is_ok()
        {
            properties.push(FilterProp::Owned {
                controller: ControllerRef::Opponent,
            });
            return own_ctrl_offset + phrase.len();
        }
    }

    let (ctrl, ctrl_len) =
        parse_controller_suffix(text).map_or((None, 0), |(ctrl, len)| (Some(ctrl), len));
    if ctrl.is_some() {
        *controller = ctrl;
    }
    ctrl_len
}

enum KeywordMatch {
    Concrete(Keyword),
    Kind(KeywordKind),
}

fn parse_leading_keyword_match(text: &str) -> Option<(KeywordMatch, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    let mut candidate_ends = vec![trimmed.len()];

    for (idx, ch) in trimmed.char_indices() {
        if matches!(ch, ' ' | ',' | '.') {
            candidate_ends.push(idx);
        }
    }

    candidate_ends.sort_unstable();
    candidate_ends.dedup();

    for end in candidate_ends.into_iter().rev() {
        let candidate = trimmed[..end].trim();
        if let Some(keyword) = parse_keyword_match(candidate) {
            return Some((keyword, leading_ws + end));
        }
    }

    None
}

fn parse_keyword_match(text: &str) -> Option<KeywordMatch> {
    if matches!(
        text,
        "flashback" | "cycling" | "escape" | "embalm" | "eternalize" | "harmonize" | "unearth"
    ) {
        let kind = match text {
            "flashback" => KeywordKind::Flashback,
            "cycling" => KeywordKind::Cycling,
            "escape" => KeywordKind::Escape,
            "embalm" => KeywordKind::Embalm,
            "eternalize" => KeywordKind::Eternalize,
            "harmonize" => KeywordKind::Harmonize,
            "unearth" => KeywordKind::Unearth,
            _ => unreachable!(),
        };
        return Some(KeywordMatch::Kind(kind));
    }

    let keyword = Keyword::from_str(text).ok()?;
    if matches!(keyword, Keyword::Unknown(_))
        && !matches!(
            text,
            "plainswalk" | "islandwalk" | "swampwalk" | "mountainwalk" | "forestwalk"
        )
    {
        return None;
    }

    Some(KeywordMatch::Concrete(keyword))
}

/// Parse "that [verb phrase]" relative clause suffix on target noun phrases.
///
/// Handles multiple pattern classes:
/// - CR 700.5: "that share(s) [a] [quality]" → `SharesQuality`
/// - CR 510.1: "that was dealt damage this turn" → `WasDealtDamageThisTurn`
/// - CR 400.7: "that entered (the battlefield) this turn" → `EnteredThisTurn`
/// - CR 508.1a: "that attacked this turn" → `AttackedThisTurn`
/// - CR 509.1a: "that blocked this turn" → `BlockedThisTurn`
///
/// Returns `(properties, bytes_consumed)` or `None` if the text doesn't match.
fn parse_that_clause_suffix(text: &str) -> Option<(Vec<FilterProp>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();

    // CR 303.4 + CR 301.5: "that's enchanted or equipped" / "that's enchanted" /
    // "that's equipped" — relative clause attaching an attachment-presence
    // predicate to the enclosing type phrase. Covers the compound-subject grant
    // class (Reyav, Master Smith; Dogmeat, Ever Loyal). Composes with disjunction
    // via `FilterProp::HasAnyAttachmentOf` (kinds.len() == 2 for the "or" form).
    if let Ok((after_contraction, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("that's ").parse(trimmed)
    {
        // Note: `parse_that_isnt_subtype_suffix` runs first in `parse_type_phrase`
        // and consumes "that's not …", so this branch only sees positive forms.
        fn parse_attachment_disjunction(
            input: &str,
        ) -> nom::IResult<&str, Vec<AttachmentKind>, nom_language::error::VerboseError<&str>>
        {
            // Longest-match-first: handle compound forms before single-kind forms.
            alt((
                value(
                    vec![AttachmentKind::Aura, AttachmentKind::Equipment],
                    tag("enchanted or equipped"),
                ),
                value(
                    vec![AttachmentKind::Equipment, AttachmentKind::Aura],
                    tag("equipped or enchanted"),
                ),
                value(vec![AttachmentKind::Aura], tag("enchanted")),
                value(vec![AttachmentKind::Equipment], tag("equipped")),
            ))
            .parse(input)
        }
        if let Ok((rest, kinds)) = parse_attachment_disjunction(after_contraction) {
            // Word-boundary check: the next char must terminate the adjective so
            // we don't false-match e.g. "that's enchanted by something else".
            // Accept end-of-string or any non-alphanumeric terminator.
            let next_char_is_boundary = rest
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if next_char_is_boundary {
                let consumed = trimmed.len() - rest.len();
                let prop = if kinds.len() == 1 {
                    FilterProp::HasAttachment {
                        kind: kinds.into_iter().next().expect("len == 1"),
                        controller: None,
                    }
                } else {
                    FilterProp::HasAnyAttachmentOf {
                        kinds,
                        controller: None,
                    }
                };
                return Some((vec![prop], leading_ws + consumed));
            }
        }
    }

    let (after_that, _) = tag::<_, _, nom_language::error::VerboseError<&str>>("that ")
        .parse(trimmed)
        .ok()?;
    let that_len = leading_ws + "that ".len();

    // --- "that share(s) [no] [a] [quality]" ---
    let share_result = nom::branch::alt((
        tag::<_, _, nom_language::error::VerboseError<&str>>("share "),
        tag("shares "),
    ))
    .parse(after_that);
    if let Ok((rest, matched_verb)) = share_result {
        let share_verb_len = matched_verb.len();

        // Optional negation: "that share no creature types"
        let (rest, _negated, neg_len) = if let Ok((r, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("no ").parse(rest)
        {
            (r, true, "no ".len())
        } else {
            (rest, false, 0)
        };

        // Optional article: "a creature type" vs "creature types"
        let (rest, a_len) = if let Ok((r, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>("a ").parse(rest)
        {
            (r, "a ".len())
        } else {
            (rest, 0)
        };

        // CR 700.5: Map quality phrase to typed SharedQuality enum.
        let quality_end = rest.find([',', '.']).unwrap_or(rest.len());
        let quality_str = rest[..quality_end].trim();
        let shared_quality = match quality_str {
            "creature type" | "creature types" => Some(SharedQuality::CreatureType),
            "color" | "colors" => Some(SharedQuality::Color),
            "card type" | "card types" => Some(SharedQuality::CardType),
            _ => None,
        };
        if let Some(quality) = shared_quality {
            let total = that_len + share_verb_len + neg_len + a_len + quality_end;
            return Some((vec![FilterProp::SharesQuality { quality }], total));
        }
    }

    // --- CR 115.9c: "that targets only [filter]" ---
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("targets only ").parse(after_that)
    {
        let targets_verb_len = "targets only ".len();
        if let Some((props, consumed)) =
            parse_targets_only_constraint(rest, that_len + targets_verb_len)
        {
            return Some((props, consumed));
        }
    }

    // --- CR 115.9b: "that targets [filter]" (.any() semantics) ---
    // Must come AFTER "targets only" check above (longest match first).
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("targets ").parse(after_that)
    {
        let targets_verb_len = "targets ".len();
        if let Some((props, consumed)) = parse_targets_constraint(rest, that_len + targets_verb_len)
        {
            return Some((props, consumed));
        }
    }

    // --- Verb-phrase patterns: match fixed phrases after "that " ---
    // CR 510.1: "that was dealt damage this turn"
    static VERB_PHRASES: &[(&str, FilterProp)] = &[
        (
            "was dealt damage this turn",
            FilterProp::WasDealtDamageThisTurn,
        ),
        (
            "entered the battlefield this turn",
            FilterProp::EnteredThisTurn,
        ),
        ("entered this turn", FilterProp::EnteredThisTurn),
        // Compound "attacked or blocked" must precede individual variants (longest match first).
        (
            "attacked or blocked this turn",
            FilterProp::AttackedOrBlockedThisTurn,
        ),
        ("attacked this turn", FilterProp::AttackedThisTurn),
        ("blocked this turn", FilterProp::BlockedThisTurn),
    ];

    for (phrase, prop) in VERB_PHRASES {
        if let Ok((_, _)) =
            tag::<_, _, nom_language::error::VerboseError<&str>>(*phrase).parse(after_that)
        {
            let total = that_len + phrase.len();
            return Some((vec![prop.clone()], total));
        }
    }

    None
}

/// CR 205.3 + CR 205.4b: "that isn't a <Subtype>" / "that's not a <Subtype>"
/// relative-clause negation suffix. Returns negated type filters to append to
/// the enclosing target's `neg_type_filters`. Mirrors the `non-<Subtype>`
/// prefix pattern but expressed as a trailing relative clause
/// ("target attacking Vampire that isn't a Demon" → `Non(Subtype("Demon"))`).
/// Composable with other suffix parsers — consumes only the "that isn't ..."
/// fragment and leaves the remainder intact.
fn parse_that_isnt_subtype_suffix(text: &str) -> Option<(Vec<TypeFilter>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();

    // "that isn't" / "that's not" / "that is not" — longest-match-first.
    let (after_neg, neg_len) = if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("that isn't ").parse(trimmed)
    {
        (rest, "that isn't ".len())
    } else if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("that's not ").parse(trimmed)
    {
        (rest, "that's not ".len())
    } else if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("that is not ").parse(trimmed)
    {
        (rest, "that is not ".len())
    } else {
        return None;
    };

    // Optional article: "a " / "an " before the subtype.
    let (after_article, article_len) = if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("a ").parse(after_neg)
    {
        (rest, "a ".len())
    } else if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("an ").parse(after_neg)
    {
        (rest, "an ".len())
    } else {
        (after_neg, 0)
    };

    // CR 205.3: Subtype token — delegates to the shared subtype recognizer.
    let (subtype, sub_len) = parse_subtype(after_article)?;
    let total = leading_ws + neg_len + article_len + sub_len;
    Some((
        vec![TypeFilter::Non(Box::new(TypeFilter::Subtype(subtype)))],
        total,
    ))
}

/// CR 115.9c: Parse the constraint after "that targets only ".
/// Returns `(properties_to_add, total_bytes_consumed)`.
///
/// Handles:
/// - "~" / "it" → `TargetsOnly { SelfRef }`
/// - "you" → `TargetsOnly { Typed { controller: You } }` (matches the player)
/// - "a single [type phrase]" → `TargetsOnly { filter }` + `HasSingleTarget`
/// - "a/an [type phrase]" → `TargetsOnly { filter }`
fn parse_targets_only_constraint(
    text: &str,
    prefix_len: usize,
) -> Option<(Vec<FilterProp>, usize)> {
    // Self-reference: "~"
    if let Ok((_, _)) = tag::<_, _, nom_language::error::VerboseError<&str>>("~").parse(text) {
        let props = vec![FilterProp::TargetsOnly {
            filter: Box::new(TargetFilter::SelfRef),
        }];
        return Some((props, prefix_len + 1));
    }
    // "it" with word boundary
    if parse_word_bounded(text, "it").is_ok() {
        let props = vec![FilterProp::TargetsOnly {
            filter: Box::new(TargetFilter::SelfRef),
        }];
        return Some((props, prefix_len + 2));
    }

    // "you" with word boundary — targets only the controller (a player)
    if parse_word_bounded(text, "you").is_ok() {
        let props = vec![FilterProp::TargetsOnly {
            filter: Box::new(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        }];
        return Some((props, prefix_len + 3));
    }

    // "a single [type phrase or player]" — TargetsOnly + HasSingleTarget
    if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("a single ").parse(text)
    {
        let single_len = "a single ".len();
        let (inner_filter, consumed) = parse_targets_only_type_or_player(rest);
        let props = vec![
            FilterProp::TargetsOnly {
                filter: Box::new(inner_filter),
            },
            FilterProp::HasSingleTarget,
        ];
        return Some((props, prefix_len + single_len + consumed));
    }

    // "a/an [type phrase or player]" — TargetsOnly without single constraint
    let article_result = nom::branch::alt((
        tag::<_, _, nom_language::error::VerboseError<&str>>("a "),
        tag("an "),
    ))
    .parse(text);
    if let Ok((rest, matched)) = article_result {
        let article_len = matched.len();
        let (inner_filter, consumed) = parse_targets_only_type_or_player(rest);
        let props = vec![FilterProp::TargetsOnly {
            filter: Box::new(inner_filter),
        }];
        return Some((props, prefix_len + article_len + consumed));
    }

    None
}

/// CR 115.9b: Parse the constraint after "that targets ".
/// Returns `(properties_to_add, total_bytes_consumed)`.
///
/// Handles:
/// - "~" / "it" / "this creature" / "this permanent" → `Targets { SelfRef }`
/// - "you" → `Targets { Controller }`
/// - "you or a [type]" → `Targets { Or(Controller, Typed) }`
/// - "one or more [type phrase]" → strip prefix, then parse type phrase
/// - "a/an [type phrase]" → `Targets { filter }`
fn parse_targets_constraint(text: &str, prefix_len: usize) -> Option<(Vec<FilterProp>, usize)> {
    // Strip "one or more " — redundant with .any() semantics
    let (text, extra_len) = if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("one or more ").parse(text)
    {
        (rest, "one or more ".len())
    } else {
        (text, 0)
    };
    let prefix_len = prefix_len + extra_len;

    // Self-reference: "~"
    if let Ok((_, _)) = tag::<_, _, nom_language::error::VerboseError<&str>>("~").parse(text) {
        let props = vec![FilterProp::Targets {
            filter: Box::new(TargetFilter::SelfRef),
        }];
        return Some((props, prefix_len + 1));
    }
    // "it" with word boundary
    if parse_word_bounded(text, "it").is_ok() {
        let props = vec![FilterProp::Targets {
            filter: Box::new(TargetFilter::SelfRef),
        }];
        return Some((props, prefix_len + 2));
    }

    // Self-reference: "this creature" / "this permanent" with word boundary
    for phrase in ["this creature", "this permanent"] {
        if parse_word_bounded(text, phrase).is_ok() {
            let props = vec![FilterProp::Targets {
                filter: Box::new(TargetFilter::SelfRef),
            }];
            return Some((props, prefix_len + phrase.len()));
        }
    }

    // "you or a [type]" / "you or an [type]" — compound controller + typed filter
    let lower = text.to_lowercase();
    let you_or_result = nom::branch::alt((
        tag::<_, _, nom_language::error::VerboseError<&str>>("you or an "),
        tag("you or a "),
    ))
    .parse(lower.as_str());
    if let Ok((_, matched)) = you_or_result {
        let you_or_len = matched.len();
        let after_you_or = &text[you_or_len..];
        let (type_filter, remainder) = parse_type_phrase(after_you_or);
        let consumed = after_you_or.len() - remainder.len();
        let combined = TargetFilter::Or {
            filters: vec![TargetFilter::Controller, type_filter],
        };
        let props = vec![FilterProp::Targets {
            filter: Box::new(combined),
        }];
        return Some((props, prefix_len + you_or_len + consumed));
    }

    // "you" — targets the controller (a player), with word boundary
    if parse_word_bounded(lower.as_str(), "you").is_ok() {
        let props = vec![FilterProp::Targets {
            filter: Box::new(TargetFilter::Controller),
        }];
        return Some((props, prefix_len + 3));
    }

    // "a/an [type phrase or player]" — parse type, using the same helper as TargetsOnly
    let article_result = nom::branch::alt((
        tag::<_, _, nom_language::error::VerboseError<&str>>("a "),
        tag("an "),
    ))
    .parse(text);
    if let Ok((rest, matched)) = article_result {
        let article_len = matched.len();
        let (inner_filter, consumed) = parse_targets_only_type_or_player(rest);
        let props = vec![FilterProp::Targets {
            filter: Box::new(inner_filter),
        }];
        return Some((props, prefix_len + article_len + consumed));
    }

    // Bare type phrase (no article) — e.g., "creatures you control"
    let (filter, remainder) = parse_type_phrase(text);
    let consumed = text.len() - remainder.len();
    if consumed > 0 {
        let props = vec![FilterProp::Targets {
            filter: Box::new(filter),
        }];
        return Some((props, prefix_len + consumed));
    }

    None
}

/// Parse the type-or-player constraint inside "that targets only a [single] ...".
/// Handles "player" as `TargetFilter::Player` and "[type] or player" as
/// `Or(Typed(type), Player)`, since `parse_type_phrase` doesn't recognize "player".
fn parse_targets_only_type_or_player(text: &str) -> (TargetFilter, usize) {
    // Check for bare "player" at start with word boundary
    if parse_word_bounded(text, "player").is_ok() {
        return (TargetFilter::Player, 6);
    }

    // Check for "[type] or player" — parse_type_phrase would consume "or" as part of
    // its compound type handling, but "player" isn't a card type, producing a broken filter.
    // Intercept this pattern: find "or player" in the text, parse only the part before it,
    // then compose with TargetFilter::Player.
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    if let Some(or_pos) = tp.find(" or player") {
        let end = or_pos + " or player".len();
        // Only match if "or player" is followed by a delimiter or end of string
        let after = &text[end..];
        match after.chars().next() {
            None | Some(',' | '.' | ' ') => {
                let type_part = tp.split_at(or_pos).0.original;
                let (type_filter, _) = parse_type_phrase(type_part);
                let combined = TargetFilter::Or {
                    filters: vec![type_filter, TargetFilter::Player],
                };
                return (combined, end);
            }
            _ => {}
        }
    }

    let (filter, remainder) = parse_type_phrase(text);
    let consumed = text.len() - remainder.len();
    (filter, consumed)
}

fn typed(
    card_type: TypeFilter,
    subtype: Option<String>,
    properties: Vec<FilterProp>,
    extra_type_filters: Vec<TypeFilter>,
) -> TargetFilter {
    let mut type_filters = vec![card_type];
    if let Some(s) = subtype {
        type_filters.push(TypeFilter::Subtype(s));
    }
    type_filters.extend(extra_type_filters);
    TargetFilter::Typed(TypedFilter {
        type_filters,
        controller: None,
        properties,
    })
}

/// Parse "the top/bottom [N] [type] card[s] of [possessive] library/graveyard".
///
/// Returns a `TargetFilter::Typed` with `InZone` for the referenced zone and the
/// appropriate controller. Matches zone position references that appear as targets
/// in exile/mill/reveal effects (e.g., "exile the top card of each player's library").
///
/// The remainder includes any trailing text after the zone word (e.g., " face down").
fn parse_zone_position_ref<'a>(text: &'a str, lower: &str) -> Option<(TargetFilter, &'a str)> {
    // Must start with "the top " or "the bottom "
    let (after_position, _is_top) = if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("the top ").parse(lower)
    {
        (rest, true)
    } else if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("the bottom ").parse(lower)
    {
        (rest, false)
    } else {
        return None;
    };

    // Optional number: "three ", "two ", "x ", etc. — skip it, we only care about the zone.
    let after_number = if let Ok((rest, _)) = nom_primitives::parse_number_or_x(after_position) {
        rest.trim_start()
    } else {
        after_position
    };

    // Optional type word before "card"/"cards": "creature card", "instant card", etc.
    let after_type = if let Ok((rest, _)) = nom_target::parse_type_filter_word(after_number) {
        let trimmed = rest.trim_start();
        // Only consume if followed by "card"/"cards" (not standalone)
        if trimmed.starts_with("card") {
            trimmed
        } else {
            after_number
        }
    } else {
        after_number
    };

    // Required "card"/"cards" — may be followed by " of [zone]" or be standalone
    let (after_card, card_is_terminal) = if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("cards").parse(after_type)
    {
        (
            rest,
            rest.trim_start().is_empty() || !rest.trim_start().starts_with("of "),
        )
    } else if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("card").parse(after_type)
    {
        (
            rest,
            rest.trim_start().is_empty() || !rest.trim_start().starts_with("of "),
        )
    } else {
        return None;
    };

    // Standalone "the top [N] cards" — default to your library
    if card_is_terminal {
        let consumed = lower.len() - after_card.len();
        return Some((
            TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::InZone {
                    zone: Zone::Library,
                }],
                ..Default::default()
            }),
            &text[consumed..],
        ));
    }

    // "of " followed by possessive + zone
    let after_of = tag::<_, _, nom_language::error::VerboseError<&str>>("of ")
        .parse(after_card.trim_start())
        .ok()?
        .0;

    // Possessive + zone word: "your library", "their library", "each player's library"
    // Try possessive first, then zone word
    let zone_words: &[(&str, &str, Zone)] = &[
        ("library", "libraries", Zone::Library),
        ("graveyard", "graveyards", Zone::Graveyard),
    ];

    // Check "each player's" / "each opponent's" / "target player's" / "target opponent's"
    let (controller, after_possessive) = if let Ok((rest, _)) =
        tag::<_, _, nom_language::error::VerboseError<&str>>("each player's ").parse(after_of)
    {
        (None, rest) // All players
    } else if let Ok((rest, _)) = alt((
        tag::<_, _, nom_language::error::VerboseError<&str>>("each opponent's "),
        tag("each opponents' "),
    ))
    .parse(after_of)
    {
        (Some(ControllerRef::Opponent), rest)
    } else if let Ok((rest, _)) = alt((
        tag::<_, _, nom_language::error::VerboseError<&str>>("target player's "),
        tag("target opponent's "),
    ))
    .parse(after_of)
    {
        (None, rest) // Targeted player — resolved at runtime
    } else if let Some((_, rest)) = strip_possessive(after_of) {
        // Generic possessive: "your library", "their library"
        let ctrl = if tag::<_, _, nom_language::error::VerboseError<&str>>("your ")
            .parse(after_of)
            .is_ok()
        {
            Some(ControllerRef::You)
        } else {
            None
        };
        (ctrl, rest)
    } else {
        return None;
    };

    // Required zone word
    for &(zone_word, zone_plural, ref zone) in zone_words {
        for word in [zone_word, zone_plural] {
            if let Ok((zone_rest, _)) =
                tag::<_, _, nom_language::error::VerboseError<&str>>(word).parse(after_possessive)
            {
                let consumed = lower.len() - zone_rest.len();
                return Some((
                    TargetFilter::Typed(TypedFilter {
                        controller,
                        properties: vec![FilterProp::InZone { zone: *zone }],
                        ..Default::default()
                    }),
                    &text[consumed..],
                ));
            }
        }
    }

    None
}

/// Preposition introducing a zone phrase. `On` is only legal for `Zone::Battlefield`
/// (CR 400.1: "on the battlefield"); other zones use `From` / `In`.
#[derive(Copy, Clone, PartialEq)]
enum ZonePrep {
    From,
    In,
    On,
}

/// Qualifier preceding the zone word. Distinguishes ownership-bearing qualifiers
/// ("an opponent's", "your") from plain determiners ("a", "the") and bare forms.
/// The `Bare` variant is a zero-width match, so `parse_zone_qual` always succeeds.
#[derive(Copy, Clone, PartialEq)]
enum ZoneQual {
    /// "an opponent's ", "each opponent's " — produces `Owned{Opponent}` per CR 404.2.
    Opponent,
    /// "your " — sets `ControllerRef::You` on the parent filter.
    You,
    /// "their ", "its owner's ", "that player's ", "defending player's ", "each player's ".
    /// No ownership constraint emitted; referent is resolved by context upstream.
    OtherPoss,
    /// "a ", "the ", or nothing (e.g., "from exile").
    Plain,
}

/// Scan `text` for the first zone phrase recognized by `parse_zone_suffix`, trying
/// position 0 and each subsequent word boundary (space-separated). Returns
/// `(Zone, Option<ControllerRef>, Vec<FilterProp>)` on the first successful parse.
///
/// Callers that already know the phrase is at the start should call `parse_zone_suffix`
/// directly; this scanner is for callers whose input has a subject before the zone
/// phrase (e.g., conditions like "this creature in your graveyard").
///
/// The returned `Zone` is extracted from the `FilterProp::InZone` entry (always present
/// in a successful parse), so callers that only need the zone don't have to pattern-match
/// the returned `Vec<FilterProp>`.
pub(crate) fn scan_zone_phrase(
    text: &str,
) -> Option<(Zone, Option<ControllerRef>, Vec<FilterProp>)> {
    let mut offset = 0;
    while offset <= text.len() {
        if let Some((props, ctrl, _consumed)) = parse_zone_suffix(&text[offset..]) {
            let zone = props.iter().find_map(|p| match p {
                FilterProp::InZone { zone } => Some(*zone),
                _ => None,
            })?;
            return Some((zone, ctrl, props));
        }
        match text[offset..].find(' ') {
            Some(i) => offset += i + 1,
            None => break,
        }
    }
    None
}

/// Parse a zone suffix like "card from a graveyard", "from your graveyard", "from exile".
///
/// Combinator structure (BNF): `[ "card" | "cards" ] prep qual zone_word`
/// - `prep`     ∈ { from, in, on }
/// - `qual`     ∈ { opponent-poss, your, other-poss, a, the, ε }
/// - `zone_word`∈ { battlefield(s), graveyard(s), exile(s), hand(s), library/libraries }
///
/// Each axis is a single `alt()` — variants are never expanded combinatorially.
///
/// Handles (CR 404.2 for opponent/owner semantics):
/// - Opponent possessive: "from an opponent's graveyard", "from each opponent's graveyard"
///   → `[Owned{Opponent}, InZone]` so stolen creatures that died are still matched by owner.
/// - Your: "from your graveyard" → `InZone` + `ControllerRef::You`.
/// - Other possessive / indefinite / definite / bare: → `InZone` alone.
pub(crate) fn parse_zone_suffix(
    text: &str,
) -> Option<(Vec<FilterProp>, Option<ControllerRef>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();
    let lower = trimmed.to_lowercase();

    let (rest, (props, ctrl)) = parse_zone_suffix_nom(&lower).ok()?;
    let consumed = lower.len() - rest.len();
    Some((props, ctrl, leading_ws + consumed))
}

fn parse_zone_suffix_nom(
    i: &str,
) -> super::oracle_nom::error::OracleResult<'_, (Vec<FilterProp>, Option<ControllerRef>)> {
    let (i, _) = opt(alt((tag("cards "), tag("card ")))).parse(i)?;
    let (i, prep) = alt((
        value(ZonePrep::From, tag("from ")),
        value(ZonePrep::In, tag("in ")),
        value(ZonePrep::On, tag("on ")),
    ))
    .parse(i)?;
    let (i, qual) = parse_zone_qual(i)?;
    let (i, zone) = parse_zone_word(i)?;
    let (i, _) = peek_zone_boundary(i)?;

    // CR 400.1: only the battlefield is referred to with "on"; "on <other zone>" is not
    // valid Oracle text, so reject it here rather than emitting a misleading filter.
    if prep == ZonePrep::On && zone != Zone::Battlefield {
        return Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                i,
                nom_language::error::VerboseErrorKind::Context("'on' requires battlefield"),
            )],
        }));
    }

    let out = match qual {
        ZoneQual::Opponent => (
            vec![
                FilterProp::Owned {
                    controller: ControllerRef::Opponent,
                },
                FilterProp::InZone { zone },
            ],
            None,
        ),
        ZoneQual::You => (vec![FilterProp::InZone { zone }], Some(ControllerRef::You)),
        ZoneQual::OtherPoss | ZoneQual::Plain => (vec![FilterProp::InZone { zone }], None),
    };
    Ok((i, out))
}

fn parse_zone_qual(i: &str) -> super::oracle_nom::error::OracleResult<'_, ZoneQual> {
    alt((
        value(
            ZoneQual::Opponent,
            alt((tag("an opponent's "), tag("each opponent's "))),
        ),
        value(ZoneQual::You, tag("your ")),
        value(
            ZoneQual::OtherPoss,
            alt((
                tag("their "),
                tag("its owner's "),
                tag("that player's "),
                tag("defending player's "),
                tag("each player's "),
            )),
        ),
        // CR 400.7: Adjective-qualified zone references — "a single graveyard" /
        // "a random graveyard" — share the indefinite-article semantics with
        // bare "a "/"the " for origin-zone tracking (the modifier constrains
        // which instance, not which zone). Longest-match-first ordering.
        value(
            ZoneQual::Plain,
            alt((tag("a single "), tag("a random "), tag("a "), tag("the "))),
        ),
        // Bare form (e.g., "from exile"): zero-width match so the zone_word combinator runs next.
        value(ZoneQual::Plain, tag("")),
    ))
    .parse(i)
}

fn parse_zone_word(i: &str) -> super::oracle_nom::error::OracleResult<'_, Zone> {
    // Longer (plural) variants precede singular so `tag` doesn't prefix-match "graveyard"
    // out of "graveyards" and leave a stray "s" that peek_zone_boundary would reject.
    alt((
        value(
            Zone::Battlefield,
            alt((tag("battlefields"), tag("battlefield"))),
        ),
        value(Zone::Graveyard, alt((tag("graveyards"), tag("graveyard")))),
        value(Zone::Exile, alt((tag("exiles"), tag("exile")))),
        value(Zone::Hand, alt((tag("hands"), tag("hand")))),
        value(Zone::Library, alt((tag("libraries"), tag("library")))),
    ))
    .parse(i)
}

/// Peek that the next character is a word boundary (end-of-string, space, comma, period).
/// Prevents matches like "graveyardkeeper" from succeeding as "graveyard".
fn peek_zone_boundary(i: &str) -> super::oracle_nom::error::OracleResult<'_, ()> {
    match i.chars().next() {
        None | Some(' ' | ',' | '.') => Ok((i, ())),
        _ => Err(nom::Err::Error(nom_language::error::VerboseError {
            errors: vec![(
                i,
                nom_language::error::VerboseErrorKind::Context("zone word boundary required"),
            )],
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_warnings::{clear_warnings, take_warnings};
    use crate::types::counter::CounterType;

    #[test]
    fn any_target() {
        let (f, rest) = parse_target("any target");
        assert_eq!(f, TargetFilter::Any);
        assert_eq!(rest, "");
    }

    #[test]
    fn target_creature() {
        let (f, _) = parse_target("target creature");
        assert_eq!(f, TargetFilter::Typed(TypedFilter::creature()));
    }

    #[test]
    fn target_creature_you_control() {
        let (f, _) = parse_target("target creature you control");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
        );
    }

    #[test]
    fn parse_target_warns_on_any_fallback() {
        clear_warnings();
        let (filter, rest) = parse_target("foobar");
        assert_eq!(filter, TargetFilter::Any);
        assert_eq!(rest, "foobar");
        assert!(take_warnings()
            .iter()
            .any(|warning| warning == "target-fallback: parse_target could not classify 'foobar'"));
    }

    #[test]
    fn attacking_creatures_you_control() {
        let (f, rest) = parse_type_phrase("attacking creatures you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Attacking])
            )
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn creature_tokens_you_control() {
        let (f, rest) = parse_type_phrase("creature tokens you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Token])
            )
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn target_nonland_permanent() {
        let (f, _) = parse_target("target nonland permanent");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent().with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
            )
        );
    }

    #[test]
    fn target_artifact_or_enchantment() {
        let (f, _) = parse_target("target artifact or enchantment");
        match f {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2);
            }
            _ => panic!("Expected Or filter, got {:?}", f),
        }
    }

    #[test]
    fn target_player() {
        let (f, _) = parse_target("target player");
        assert_eq!(f, TargetFilter::Player);
    }

    #[test]
    fn bare_player_is_player_target() {
        let (f, rest) = parse_target("player, choose a creature card in that player's graveyard");
        assert_eq!(f, TargetFilter::Player);
        assert_eq!(rest, ", choose a creature card in that player's graveyard");
    }

    #[test]
    fn bare_graveyards_are_cards_in_graveyard_zone() {
        let (f, rest) = parse_target("graveyards");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }]))
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn bare_him_inherits_parent_target() {
        let (f, rest) = parse_target("him");
        assert_eq!(f, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn bare_her_inherits_parent_target() {
        let (f, rest) = parse_target("her");
        assert_eq!(f, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn on_it_inherits_parent_target() {
        let (f, rest) = parse_target("on it");
        assert_eq!(f, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn bare_one_inherits_parent_target() {
        let (f, rest) = parse_target("one into your hand");
        assert_eq!(f, TargetFilter::ParentTarget);
        assert_eq!(rest, " into your hand");
    }

    #[test]
    fn tap_or_untap_target_permanent_strips_verb_prefix() {
        let (f, rest) = parse_target("or untap target permanent");
        assert_eq!(f, TargetFilter::Typed(TypedFilter::permanent()));
        assert_eq!(rest, "");
    }

    #[test]
    fn target_count_placeholders_map_to_any_target() {
        let (f, rest) = parse_target("one or two targets");
        assert_eq!(f, TargetFilter::Any);
        assert_eq!(rest, "");
    }

    #[test]
    fn quantified_of_them_produces_tracked_set() {
        let (f, rest) = parse_target("two of them");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn quantified_cards_from_hand_parse_as_zone_filter() {
        let (f, rest) = parse_target("two cards from your hand");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::card()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone { zone: Zone::Hand }])
            )
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn enchanted_creature() {
        let (f, _) = parse_target("enchanted creature");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]))
        );
    }

    #[test]
    fn enchanted_permanent() {
        let (f, _) = parse_target("enchanted permanent");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy]))
        );
    }

    #[test]
    fn enchanted_permanents_controller() {
        let (f, _) = parse_target("enchanted permanent's controller");
        assert_eq!(f, TargetFilter::ParentTargetController);
    }

    #[test]
    fn equipped_creature() {
        let (f, _) = parse_target("equipped creature");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy]))
        );
    }

    #[test]
    fn each_opponent() {
        let (f, _) = parse_target("each opponent");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
    }

    #[test]
    fn target_opponent() {
        let (f, _) = parse_target("target opponent");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
    }

    #[test]
    fn or_type_distributes_controller() {
        // "creature or artifact you control" → both branches get You controller
        let (f, _) = parse_target("target creature or artifact you control");
        match f {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You)
                    )
                );
            }
            _ => panic!("Expected Or filter, got {:?}", f),
        }
    }

    #[test]
    fn tilde_is_self_ref() {
        let (f, rest) = parse_target("~");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, "");
    }

    #[test]
    fn tilde_with_trailing_text() {
        let (f, rest) = parse_target("~ to its owner's hand");
        assert_eq!(f, TargetFilter::SelfRef);
        assert!(rest.contains("to its owner"));
    }

    #[test]
    fn this_creature_is_self_ref() {
        let (f, rest) = parse_target("this creature to its owner's hand");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, " to its owner's hand");
    }

    #[test]
    fn this_creature_exact_is_self_ref() {
        let (f, rest) = parse_target("this creature");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, "");
    }

    #[test]
    fn this_permanent_is_self_ref() {
        let (f, rest) = parse_target("this permanent");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, "");
    }

    #[test]
    fn this_enchantment_is_self_ref() {
        let (f, rest) = parse_target("this enchantment");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, "");
    }

    #[test]
    fn white_creature_you_control() {
        let (f, _) = parse_type_phrase("white creature you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::HasColor {
                        color: ManaColor::White
                    }])
            )
        );
    }

    #[test]
    fn red_spell() {
        let (f, _) = parse_type_phrase("red spell");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::HasColor {
                color: ManaColor::Red
            }]))
        );
    }

    /// CR 208.1: "creature with power or toughness N or less" produces a
    /// disjunctive `AnyOf { [PowerLE, ToughnessLE] }` property. Used by
    /// Arnyn Deathbloom Botanist's dies-trigger subject filter, Stern
    /// Scolding's counter target, Warping Wail mode 1, etc.
    #[test]
    fn creature_with_power_or_toughness_1_or_less() {
        let (f, _) = parse_type_phrase("creature with power or toughness 1 or less");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::AnyOf {
                props: vec![
                    FilterProp::PowerLE {
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                    FilterProp::ToughnessLE {
                        value: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            }]))
        );
    }

    /// Disjunctive "or greater" form, mirror of the "or less" case.
    #[test]
    fn creature_with_power_or_toughness_3_or_greater() {
        let (f, _) = parse_type_phrase("creature with power or toughness 3 or greater");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::AnyOf {
                props: vec![
                    FilterProp::PowerGE {
                        value: QuantityExpr::Fixed { value: 3 },
                    },
                    FilterProp::ToughnessGE {
                        value: QuantityExpr::Fixed { value: 3 },
                    },
                ],
            }]))
        );
    }

    /// Standalone "with toughness N or less" — new parser branch, mirror of
    /// the pre-existing "with power N or less" form.
    #[test]
    fn creature_with_toughness_2_or_less() {
        let (f, _) = parse_type_phrase("creature with toughness 2 or less");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::ToughnessLE {
                    value: QuantityExpr::Fixed { value: 2 },
                }
            ]))
        );
    }

    #[test]
    fn spell_with_mana_value_4_or_greater() {
        let (f, _) = parse_type_phrase("spell with mana value 4 or greater");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::CmcGE {
                value: QuantityExpr::Fixed { value: 4 },
            }]))
        );
    }

    /// CR 107.3a + CR 601.2b: Nature's Rhythm — "creature card with mana value X
    /// or less". The literal X must produce a `QuantityRef::Variable { "X" }`,
    /// resolved at effect time against the spell's announced X.
    #[test]
    fn creature_with_mana_value_x_or_less() {
        let (f, _) = parse_type_phrase("creature card with mana value x or less");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::CmcLE {
                value: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]))
        );
    }

    #[test]
    fn spell_with_mana_value_x_or_greater() {
        let (f, _) = parse_type_phrase("spell with mana value x or greater");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::CmcGE {
                value: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]))
        );
    }

    #[test]
    fn creature_you_control_with_power_2_or_less() {
        let (f, rest) = parse_type_phrase("creature you control with power 2 or less enter");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::PowerLE {
                        value: QuantityExpr::Fixed { value: 2 }
                    }])
            )
        );
        // Remaining text should be the event verb
        assert!(rest.trim_start().starts_with("enter"), "rest = {:?}", rest);
    }

    #[test]
    fn creature_with_power_3_or_greater() {
        let (f, _) = parse_type_phrase("creature with power 3 or greater");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::PowerGE {
                    value: QuantityExpr::Fixed { value: 3 }
                }])
            )
        );
    }

    #[test]
    fn creature_with_power_x_or_less() {
        // CR 107.3a + CR 601.2b: X is announced at cast; the filter retains the
        // `Variable("X")` marker so it can resolve against `chosen_x` at effect time.
        let (prop, _) = parse_power_suffix("with power x or less").expect("parses");
        assert_eq!(
            prop,
            FilterProp::PowerLE {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string()
                    }
                }
            }
        );
    }

    #[test]
    fn creature_with_power_x_or_greater() {
        let (prop, _) = parse_power_suffix("with power x or greater").expect("parses");
        assert_eq!(
            prop,
            FilterProp::PowerGE {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string()
                    }
                }
            }
        );
    }

    #[test]
    fn creatures_with_ice_counters_on_them() {
        let (f, _) = parse_type_phrase("creatures with ice counters on them");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::CountersGE {
                    counter_type: CounterType::Generic("ice".to_string()),
                    count: QuantityExpr::Fixed { value: 1 },
                },])
            )
        );
    }

    #[test]
    fn cards_in_graveyards() {
        let (f, _) = parse_type_phrase("cards in graveyards");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }]))
        );
    }

    #[test]
    fn target_card_from_a_graveyard() {
        let (f, rest) = parse_target("target card from a graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard
            }]))
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn elf_on_the_battlefield() {
        let (f, rest) = parse_type_phrase("Elf on the battlefield");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Elf".to_string())
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Battlefield,
                    }],)
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_creature_card_in_your_graveyard() {
        let (f, rest) = parse_target("target creature card in your graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Graveyard
                    }])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_card_from_exile() {
        let (f, rest) = parse_target("target card from exile");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::InZone { zone: Zone::Exile }])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_card_in_a_graveyard() {
        let (f, _) = parse_target("target card in a graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard
            }]))
        );
    }

    #[test]
    fn card_with_flashback_uses_keyword_kind_filter() {
        let (f, _) = parse_type_phrase("card with flashback");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::card().properties(vec![FilterProp::HasKeywordKind {
                    value: KeywordKind::Flashback,
                },])
            )
        );
    }

    #[test]
    fn cards_with_flashback_you_own_in_exile() {
        let (f, _) = parse_type_phrase("cards with flashback you own in exile");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::HasKeywordKind {
                    value: KeywordKind::Flashback,
                },
                FilterProp::Owned {
                    controller: ControllerRef::You,
                },
                FilterProp::InZone { zone: Zone::Exile },
            ]))
        );
    }

    #[test]
    fn creature_of_the_chosen_type() {
        let (f, _) = parse_type_phrase("creature you control of the chosen type");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::IsChosenCreatureType])
            )
        );
    }

    #[test]
    fn creatures_you_control_with_flying() {
        let (f, _) = parse_type_phrase("creatures you control with flying");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::WithKeyword {
                        value: Keyword::Flying,
                    }])
            )
        );
    }

    #[test]
    fn creature_with_first_strike_and_vigilance() {
        let (f, _) = parse_type_phrase("creature with first strike and vigilance");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::WithKeyword {
                    value: Keyword::FirstStrike,
                },
                FilterProp::WithKeyword {
                    value: Keyword::Vigilance,
                },
            ]))
        );
    }

    #[test]
    fn other_nonland_permanents_you_own_and_control() {
        let (f, _) = parse_type_phrase("other nonland permanents you own and control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
                    .properties(vec![
                        FilterProp::Another,
                        FilterProp::Owned {
                            controller: ControllerRef::You,
                        },
                    ])
            )
        );
    }

    #[test]
    fn permanents_you_own() {
        let (f, _) = parse_type_phrase("permanents you own");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::Owned {
                controller: ControllerRef::You,
            }]))
        );
    }

    #[test]
    fn other_creatures_you_control() {
        let (f, _) = parse_type_phrase("other creatures you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Another])
            )
        );
    }

    // ── Anaphoric pronouns (Building Block C) ──

    #[test]
    fn those_cards_produces_tracked_set() {
        let (f, rest) = parse_target("those cards");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn the_rest_produces_tracked_set() {
        let (f, rest) = parse_target("the rest");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn both_cards_produces_tracked_set() {
        // CR 608.2c: Sword of Hearth and Home — "exile up to one target
        // creature you own, then search your library for a basic land card.
        // Put both cards onto the battlefield under your control." "both
        // cards" is an anaphoric back-reference to the exiled creature + the
        // searched land, both published into the chain-scoped tracked set.
        let (f, rest) = parse_target("both cards");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn those_tokens_produces_tracked_set() {
        let (f, rest) = parse_target("those tokens");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn those_lands_produce_tracked_set() {
        let (filter, rest) = parse_target("those lands");
        assert_eq!(
            filter,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn the_token_inherits_parent_target() {
        let (filter, rest) = parse_target("the token");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn the_chosen_creature_inherits_parent_target() {
        let (filter, rest) = parse_target("the chosen creature");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn the_chosen_card_inherits_parent_target() {
        let (filter, rest) = parse_target("the chosen card");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn the_chosen_cards_produce_tracked_set() {
        let (filter, rest) = parse_target("the chosen cards");
        assert_eq!(
            filter,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn one_of_them_inherits_parent_target() {
        let (filter, rest) = parse_target("one of them");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn one_of_those_cards_inherits_parent_target() {
        let (filter, rest) = parse_target("one of those cards");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn new_targets_for_the_copy_inherits_parent_target() {
        let (filter, rest) = parse_target("new targets for the copy");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn new_targets_for_it_inherits_parent_target() {
        let (filter, rest) = parse_target("new targets for it");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn up_to_one_of_them_inherits_parent_target() {
        let (filter, rest) = parse_target("up to one of them");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn either_of_them_inherits_parent_target() {
        let (filter, rest) = parse_target("either of them");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn quantified_target_phrase_strips_prefix() {
        let (filter, rest) = parse_target("one or two target creatures");
        assert_eq!(filter, TargetFilter::Typed(TypedFilter::creature()));
        assert_eq!(rest, "");
    }

    #[test]
    fn quantified_up_to_target_phrase_strips_prefix() {
        let (filter, rest) = parse_target("up to one target creature you control");
        assert_eq!(
            filter,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn quantified_x_target_phrase_strips_prefix() {
        let (filter, rest) = parse_target("X target creature cards from your graveyard");
        let TargetFilter::Typed(tf) = filter else {
            panic!("Expected Typed filter");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.contains(&FilterProp::InZone {
            zone: Zone::Graveyard
        }));
        assert_eq!(rest, "");
    }

    #[test]
    fn of_them_produces_tracked_set() {
        let (filter, rest) = parse_target("of them");
        assert_eq!(
            filter,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn the_exiled_card_produces_tracked_set() {
        let (f, _) = parse_target("the exiled card");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
    }

    #[test]
    fn the_exiled_permanents_produces_tracked_set() {
        let (f, _) = parse_target("the exiled permanents");
        assert_eq!(
            f,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
        );
    }

    // ── ExiledBySource ──

    #[test]
    fn each_card_exiled_with_tilde_produces_exiled_by_source() {
        let (f, rest) = parse_target("each card exiled with ~ into its owner's graveyard");
        assert_eq!(f, TargetFilter::ExiledBySource);
        assert_eq!(rest, " into its owner's graveyard");
    }

    #[test]
    fn parse_target_it_inherits_parent_target() {
        let (filter, rest) = parse_target("it");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_them_inherits_parent_target() {
        let (filter, rest) = parse_target("them");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_that_spell_inherits_parent_target() {
        let (filter, rest) = parse_target("that spell is countered this way");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, " is countered this way");
    }

    #[test]
    fn parse_target_that_creature_inherits_parent_target() {
        // CR 608.2c: Without trigger context, "that creature" defaults to the
        // parent target (Twinflame Strive: "create a token that's a copy of that
        // creature"). Trigger-context resolution to `TriggeringSource` is layered
        // on top of `parse_target` by callers that thread a `ParseContext` (see
        // `resolve_counter_placement_target` in `oracle_effect/counter.rs`).
        let (filter, rest) = parse_target("that creature");
        assert_eq!(filter, TargetFilter::ParentTarget);
        assert_eq!(rest, "");
    }

    #[test]
    fn each_card_exiled_with_this_artifact_produces_exiled_by_source() {
        let (f, rest) = parse_target("each card exiled with this artifact");
        assert_eq!(f, TargetFilter::ExiledBySource);
        assert_eq!(rest, "");
    }

    #[test]
    fn cards_exiled_with_tilde_produces_exiled_by_source() {
        let (f, _) = parse_target("cards exiled with ~");
        assert_eq!(f, TargetFilter::ExiledBySource);
    }

    #[test]
    fn target_creature_card_exiled_with_tilde_produces_and_filter() {
        // CR 406.6: Singular targeted form — composes typed filter with the
        // exile-link constraint via TargetFilter::And.
        let (f, rest) = parse_target("target creature card exiled with ~");
        assert_eq!(
            f,
            TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature()),
                    TargetFilter::ExiledBySource,
                ],
            }
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_creature_card_exiled_with_this_creature_produces_and_filter() {
        let (f, rest) = parse_target("target creature card exiled with this creature");
        assert_eq!(
            f,
            TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature()),
                    TargetFilter::ExiledBySource,
                ],
            }
        );
        assert_eq!(rest.trim(), "");
    }

    // ── "from a single graveyard" zone qualifier ──

    #[test]
    fn target_card_from_a_single_graveyard() {
        // CR 400.7: "a single graveyard" shares origin-zone semantics with
        // bare "a graveyard"; the modifier constrains which instance, not
        // which zone.
        let (f, rest) = parse_target("target card from a single graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard
            }]))
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn up_to_two_target_cards_from_a_single_graveyard() {
        // Hearse activated ability target text after "exile " is stripped.
        let (f, rest) = parse_target("up to two target cards from a single graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard
            }]))
        );
        assert_eq!(rest.trim(), "");
    }

    // ── Bare type phrase fallback ──

    #[test]
    fn bare_type_phrase_fallback() {
        let (f, _) = parse_target("other nonland permanents you own and control");
        // Should be Typed (not Any) — parse_type_phrase picks up the permanent type + properties
        match f {
            TargetFilter::Typed(tf) => {
                assert!(
                    !tf.type_filters.is_empty() || !tf.properties.is_empty(),
                    "Expected meaningful type info, got {:?}",
                    tf
                );
            }
            other => panic!("Expected Typed, got {:?}", other),
        }
    }

    #[test]
    fn unrecognized_bare_text_stays_any() {
        let (f, _) = parse_target("foobar");
        assert_eq!(f, TargetFilter::Any);
    }

    #[test]
    fn parse_event_context_that_spells_controller() {
        let (filter, rem) = parse_event_context_ref("that spell's controller").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringSpellController);
        assert_eq!(rem, "");
    }

    #[test]
    fn parse_event_context_that_spells_owner() {
        let (filter, rem) = parse_event_context_ref("that spell's owner").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringSpellOwner);
        assert_eq!(rem, "");
    }

    #[test]
    fn parse_event_context_that_player() {
        let (filter, rem) = parse_event_context_ref("that player").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringPlayer);
        assert_eq!(rem, "");
    }

    #[test]
    fn parse_event_context_that_source() {
        let (filter, rem) = parse_event_context_ref("that source").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringSource);
        assert_eq!(rem, "");
    }

    #[test]
    fn parse_event_context_that_permanent() {
        let (filter, rem) = parse_event_context_ref("that permanent").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringSource);
        assert_eq!(rem, "");
    }

    #[test]
    fn parse_event_context_returns_none_for_non_event() {
        assert_eq!(parse_event_context_ref("target creature"), None);
        assert_eq!(parse_event_context_ref("any target"), None);
    }

    #[test]
    fn parse_event_context_defending_player() {
        let (filter, rem) = parse_event_context_ref("defending player").unwrap();
        assert_eq!(filter, TargetFilter::DefendingPlayer);
        assert_eq!(rem, "");
    }

    #[test]
    fn parse_event_context_defending_player_prefix() {
        let (filter, rem) =
            parse_event_context_ref("defending player reveals the top card").unwrap();
        assert_eq!(filter, TargetFilter::DefendingPlayer);
        assert_eq!(rem, " reveals the top card");
    }

    #[test]
    fn event_context_ref_preserves_remainder() {
        // Compound remainder preserved with leading space
        let (filter, rem) = parse_event_context_ref("that player and you gain 2 life").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringPlayer);
        assert_eq!(rem, " and you gain 2 life");

        // "that permanent or player" — longest-match-first, no bogus " or player" remainder
        let (filter, rem) =
            parse_event_context_ref("that permanent or player and the damage can't be prevented")
                .unwrap();
        assert_eq!(filter, TargetFilter::TriggeringSource);
        assert_eq!(rem, " and the damage can't be prevented");

        // "that source" with remainder
        let (filter, rem) = parse_event_context_ref("that source and you draw a card").unwrap();
        assert_eq!(filter, TargetFilter::TriggeringSource);
        assert_eq!(rem, " and you draw a card");
    }

    #[test]
    fn parse_counter_suffix_stun_counter() {
        let result = parse_counter_suffix(" with a stun counter on it");
        assert!(result.is_some());
        let (prop, _consumed) = result.unwrap();
        assert!(matches!(
            prop,
            FilterProp::CountersGE {
                counter_type: CounterType::Stun,
                count: QuantityExpr::Fixed { value: 1 },
            }
        ));
    }

    #[test]
    fn parse_counter_suffix_oil_counter() {
        let result = parse_counter_suffix(" with an oil counter on it");
        assert!(result.is_some());
        let (prop, _consumed) = result.unwrap();
        assert!(matches!(
            prop,
            FilterProp::CountersGE {
                counter_type: CounterType::Generic(ref s),
                count: QuantityExpr::Fixed { value: 1 },
            } if s == "oil"
        ));
    }

    #[test]
    fn parse_counter_suffix_not_counter_phrase() {
        let result = parse_counter_suffix(" with power 3 or greater");
        assert!(result.is_none());
    }

    #[test]
    fn parse_type_phrase_creature_with_stun_counter() {
        let (filter, _rest) = parse_type_phrase("creature with a stun counter on it");
        match filter {
            TargetFilter::Typed(ref tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::CountersGE {
                        ref counter_type,
                        count: QuantityExpr::Fixed { value: 1 },
                    } if *counter_type == CounterType::Stun
                )));
            }
            other => panic!("Expected Typed, got {:?}", other),
        }
    }

    #[test]
    fn creatures_your_opponents_control() {
        let (f, rest) = parse_type_phrase("creatures your opponents control");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent))
        );
        assert_eq!(rest.trim(), "");
    }

    /// CR 109.4 + CR 115.1: "other creature target player controls" produces
    /// a filter scoped to a chosen player target. The companion
    /// `TargetFilter::Player` target slot is surfaced by `collect_target_slots`
    /// in the engine at target-declaration time; this parser test just verifies
    /// the filter's controller marker is `TargetPlayer` and the `other` modifier
    /// is preserved.
    #[test]
    fn other_creature_target_player_controls() {
        let (f, rest) = parse_type_phrase("other creature target player controls");
        match f {
            TargetFilter::Typed(ref tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::TargetPlayer));
                assert!(
                    tf.properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::Another)),
                    "expected `Another` property for `other` modifier, got {:?}",
                    tf.properties
                );
            }
            other => panic!("Expected Typed filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    /// Sibling coverage: bare "creatures target player controls" without
    /// "each other" prefix. Confirms the controller parser is independent of
    /// modifier words.
    #[test]
    fn creatures_target_player_controls() {
        let (f, rest) = parse_type_phrase("creatures target player controls");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::TargetPlayer))
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn artifacts_and_creatures_your_opponents_control() {
        let (f, rest) = parse_type_phrase("artifacts and creatures your opponents control");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent)
                    )
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn creature_an_opponent_controls_still_works() {
        let (f, rest) = parse_type_phrase("creature an opponent controls");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent))
        );
        assert_eq!(rest.trim(), "");
    }

    // CR 205.3a: Comma-separated type list tests

    #[test]
    fn comma_list_three_types_with_opponent_control() {
        let (f, rest) = parse_type_phrase("artifacts, creatures, and lands your opponents control");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Land).controller(ControllerRef::Opponent)
                    )
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn comma_list_three_types_no_controller() {
        let (f, rest) = parse_type_phrase("artifacts, creatures, and enchantments");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))
                );
                assert_eq!(filters[1], TargetFilter::Typed(TypedFilter::creature()));
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment))
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn comma_list_you_control() {
        let (f, rest) = parse_type_phrase("creatures, artifacts, and enchantments you control");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You)
                    )
                );
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Enchantment).controller(ControllerRef::You)
                    )
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn modified_adjective_creates_filter_prop() {
        // CR 700.4 + CR 700.9: "modified creature" is a first-class adjective
        // attaching FilterProp::Modified to a typed creature filter.
        let (f, rest) = parse_type_phrase("modified creature you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Modified])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn modified_adjective_in_comma_list_silkguard() {
        // CR 700.4 + CR 700.9: Silkguard — "Auras, Equipment, and modified
        // creatures you control gain hexproof". The subject is a three-way OR
        // of Aura (subtype), Equipment (subtype), and creature-with-Modified.
        // The trailing "you control" controller scope distributes across all
        // three legs via `distribute_controller_to_or`.
        let (f, rest) = parse_type_phrase("auras, equipment, and modified creatures you control");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3, "expected 3-way OR, got {filters:#?}");
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(
                        TypedFilter::default()
                            .subtype("Aura".to_string())
                            .controller(ControllerRef::You)
                    ),
                    "leg 0 = Auras you control"
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::default()
                            .subtype("Equipment".to_string())
                            .controller(ControllerRef::You)
                    ),
                    "leg 1 = Equipment you control"
                );
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::Modified])
                    ),
                    "leg 2 = modified creatures you control"
                );
            }
            other => panic!("Expected Or filter, got {other:?}"),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn historic_adjective_creates_filter_prop() {
        // CR 700.6: "historic permanent" is a first-class adjective attaching
        // FilterProp::Historic to a typed permanent filter.
        let (f, rest) = parse_type_phrase("historic permanent you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Historic])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn historic_adjective_after_nontoken_arbaaz() {
        // CR 700.6: Arbaaz Mir's "another nontoken historic permanent you
        // control" composes negation (`Non(Token)` subtype), the Historic
        // adjective, the Another property, and the You controller — all in
        // sequence. The historic adjective parses AFTER the `non` negation
        // sweep, exercising the post-negation arm.
        let (f, rest) = parse_type_phrase("another nontoken historic permanent you control");
        match f {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.type_filters.contains(&TypeFilter::Permanent),
                    "expected Permanent in {:?}",
                    tf.type_filters,
                );
                assert!(
                    tf.type_filters
                        .contains(&TypeFilter::Non(Box::new(TypeFilter::Subtype(
                            "Token".to_string()
                        )))),
                    "expected Non(Token) in {:?}",
                    tf.type_filters,
                );
                assert!(
                    tf.properties.contains(&FilterProp::Historic),
                    "expected Historic in {:?}",
                    tf.properties,
                );
                assert!(
                    tf.properties.contains(&FilterProp::Another),
                    "expected Another in {:?}",
                    tf.properties,
                );
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn historic_adjective_does_not_propagate_to_or_legs() {
        // CR 700.6 + CR 700.4: `FilterProp::Historic` is leg-local — in a
        // comma OR list it must NOT distribute back to earlier legs. Mirrors
        // the Modified adjective handling for Silkguard.
        let (f, _rest) = parse_type_phrase("artifacts and historic creatures you control");
        let TargetFilter::Or { ref filters } = f else {
            panic!("Expected Or filter, got {f:?}");
        };
        let leg_has_historic = |idx: usize| -> bool {
            matches!(
                filters.get(idx),
                Some(TargetFilter::Typed(tf)) if tf.properties.contains(&FilterProp::Historic)
            )
        };
        assert!(
            !leg_has_historic(0),
            "Historic must not propagate to artifact leg in {filters:#?}",
        );
        assert!(
            leg_has_historic(filters.len() - 1),
            "creature leg must keep Historic in {filters:#?}",
        );
    }

    #[test]
    fn comma_list_four_elements() {
        let (f, rest) = parse_type_phrase("artifacts, creatures, enchantments, and lands");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 4);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))
                );
                assert_eq!(filters[1], TargetFilter::Typed(TypedFilter::creature()));
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment))
                );
                assert_eq!(
                    filters[3],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Land))
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn comma_list_no_oxford_comma() {
        let (f, rest) = parse_type_phrase("artifacts, creatures and lands your opponents control");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Land).controller(ControllerRef::Opponent)
                    )
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn comma_list_remainder() {
        let (f, rest) = parse_type_phrase("artifacts, creatures, and lands enter tapped");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 3);
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
        assert_eq!(rest, " enter tapped");
    }

    // ── Feature 1: Stacked negation ──

    #[test]
    fn noncreature_nonland_permanent() {
        let (f, rest) = parse_type_phrase("noncreature, nonland permanent");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent()
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Creature)))
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn noncreature_nonland_permanents_you_control() {
        let (f, rest) = parse_type_phrase("noncreature, nonland permanents you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent()
                    .controller(ControllerRef::You)
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Creature)))
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn nonartifact_nonblack_creature() {
        // CR 205.4b: "nonartifact" → Non(Artifact) in type_filters, "nonblack" → NotColor in properties
        let (f, rest) = parse_type_phrase("nonartifact, nonblack creature");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::creature()
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Artifact)))
                    .properties(vec![FilterProp::NotColor {
                        color: ManaColor::Black,
                    },])
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn triple_stacked_negation() {
        let (f, _) = parse_type_phrase("noncreature, nonland, nonartifact permanent");
        match f {
            TargetFilter::Typed(ref tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Permanent));
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Creature))));
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Artifact))));
            }
            other => panic!("Expected Typed, got {:?}", other),
        }
    }

    // ── Feature 1: starts_with_type_word guard ──

    #[test]
    fn starts_with_type_word_core_types() {
        assert!(starts_with_type_word("creatures"));
        assert!(starts_with_type_word("artifact"));
        assert!(starts_with_type_word("permanents you control"));
    }

    #[test]
    fn starts_with_type_word_negated() {
        assert!(starts_with_type_word("noncreature spell"));
        assert!(starts_with_type_word("nonland permanent"));
    }

    #[test]
    fn starts_with_type_word_subtypes() {
        assert!(starts_with_type_word("zombie"));
        assert!(starts_with_type_word("vampires"));
        assert!(starts_with_type_word("elves"));
    }

    #[test]
    fn starts_with_type_word_rejects_non_types() {
        assert!(!starts_with_type_word("draw a card"));
        assert!(!starts_with_type_word("destroy target"));
        assert!(!starts_with_type_word("you control"));
    }

    // ── Feature 2: Subtype recognition ──

    #[test]
    fn zombies_you_control() {
        let (f, rest) = parse_type_phrase("zombies you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Zombie".to_string())
                    .controller(ControllerRef::You)
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn elves_you_control_irregular_plural() {
        let (f, rest) = parse_type_phrase("elves you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Elf".to_string())
                    .controller(ControllerRef::You)
            )
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn equipment_subtype() {
        let (f, _) = parse_type_phrase("equipment you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Equipment".to_string())
                    .controller(ControllerRef::You)
            )
        );
    }

    #[test]
    fn forest_land_subtype() {
        let (f, _) = parse_type_phrase("forest");
        match f {
            TargetFilter::Typed(ref tf) => {
                assert_eq!(tf.get_subtype(), Some("Forest"));
            }
            other => panic!("Expected Typed, got {:?}", other),
        }
    }

    // ── Feature 3: Supertype prefixes ──

    #[test]
    fn legendary_creature() {
        let (f, _) = parse_type_phrase("legendary creature");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::HasSupertype {
                    value: Supertype::Legendary,
                }
            ]))
        );
    }

    #[test]
    fn basic_lands_you_control() {
        let (f, _) = parse_type_phrase("basic lands you control");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::land()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::HasSupertype {
                        value: Supertype::Basic,
                    }])
            )
        );
    }

    #[test]
    fn parse_target_article_basic_land_you_control() {
        let (filter, rest) = parse_target("a basic land you control");
        assert_eq!(
            filter,
            TargetFilter::Typed(
                TypedFilter::land()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::HasSupertype {
                        value: Supertype::Basic,
                    }])
            )
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_article_basic_land_card_from_hand() {
        let (filter, rest) = parse_target("a basic land card from your hand");
        assert_eq!(
            filter,
            TargetFilter::Typed(
                TypedFilter::land()
                    .controller(ControllerRef::You)
                    .properties(vec![
                        FilterProp::HasSupertype {
                            value: Supertype::Basic,
                        },
                        FilterProp::InZone { zone: Zone::Hand },
                    ])
            )
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn snow_permanents() {
        let (f, _) = parse_type_phrase("snow permanents");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                FilterProp::HasSupertype {
                    value: Supertype::Snow,
                }
            ]))
        );
    }

    #[test]
    fn legendary_white_creature() {
        // CR 205.4a: Supertype + color compose in properties
        let (f, _) = parse_type_phrase("legendary white creature");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter::creature().properties(vec![
                FilterProp::HasSupertype {
                    value: Supertype::Legendary
                },
                FilterProp::HasColor {
                    color: ManaColor::White
                },
            ]))
        );
    }

    #[test]
    fn nonbasic_land() {
        // CR 205.4a: "nonbasic" → NotSupertype (property), not TypeFilter::Non
        let (f, _) = parse_type_phrase("nonbasic land");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::land().properties(vec![FilterProp::NotSupertype {
                    value: Supertype::Basic,
                }])
            )
        );
    }

    #[test]
    fn nonbasic_lands_opponent_controls() {
        let (f, _) = parse_type_phrase("nonbasic lands an opponent controls");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::land()
                    .controller(ControllerRef::Opponent)
                    .properties(vec![FilterProp::NotSupertype {
                        value: Supertype::Basic,
                    }])
            )
        );
    }

    // ── Feature 4: "and/or" separator ──

    /// CR 608.2b: "creature and/or land" composes via existing "and/or"
    /// support to `TargetFilter::Or { [Creature, Land] }`. Regression guard
    /// for Zimone's Experiment: the compound type filter on Dig's reveal
    /// gate must produce `Or` (not drop to `Any`) so the Dig's `filter`
    /// correctly restricts the player's selectable set during DigChoice.
    #[test]
    fn creature_and_or_land_composes_to_or_filter() {
        let (f, _) = parse_type_phrase("creature and/or land");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature))
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Land))
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
    }

    #[test]
    fn artifact_and_or_enchantment() {
        let (f, _) = parse_type_phrase("artifact and/or enchantment");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment))
                );
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
    }

    #[test]
    fn instant_and_or_sorcery() {
        let (f, _) = parse_type_phrase("instant and/or sorcery");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2);
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
    }

    #[test]
    fn creature_and_or_planeswalker_you_control() {
        let (f, _) = parse_type_phrase("creature and/or planeswalker you control");
        match f {
            TargetFilter::Or { ref filters } => {
                assert_eq!(filters.len(), 2);
                // Both branches should have controller distributed
                for filter in filters {
                    if let TargetFilter::Typed(typed) = filter {
                        assert_eq!(typed.controller, Some(ControllerRef::You));
                    } else {
                        panic!("Expected Typed in Or, got {:?}", filter);
                    }
                }
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
    }

    // ── Regression: existing tests still pass with new features ──

    #[test]
    fn existing_nonland_still_works() {
        // Single non-prefix (not stacked) should work as before
        let (f, _) = parse_type_phrase("nonland permanent");
        assert_eq!(
            f,
            TargetFilter::Typed(
                TypedFilter::permanent().with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
            )
        );
    }

    #[test]
    fn and_still_works_with_non_type_text() {
        // "creature and draw a card" — "and" should NOT recurse because "draw" isn't a type
        let (f, rest) = parse_type_phrase("creature and draw a card");
        assert_eq!(f, TargetFilter::Typed(TypedFilter::creature()));
        assert!(rest.contains("and draw"), "rest = {:?}", rest);
    }

    #[test]
    fn distribute_properties_across_or_branches() {
        // "artifacts and creatures with mana value 2 or less" → both branches get CmcLE(2)
        let (f, _) = parse_type_phrase("artifacts and creatures with mana value 2 or less");
        if let TargetFilter::Or { filters } = &f {
            assert_eq!(filters.len(), 2, "should have 2 Or branches");
            for branch in filters {
                if let TargetFilter::Typed(typed) = branch {
                    assert!(
                        typed.properties.iter().any(|p| matches!(
                            p,
                            FilterProp::CmcLE {
                                value: QuantityExpr::Fixed { value: 2 }
                            }
                        )),
                        "branch {:?} should have CmcLE(2)",
                        typed.get_primary_type()
                    );
                } else {
                    panic!("expected Typed branch, got {branch:?}");
                }
            }
        } else {
            panic!("expected Or filter, got {f:?}");
        }
    }

    #[test]
    fn parse_type_phrase_ninja_or_rogue_creatures_you_control() {
        // CR 205.3a: "ninja or rogue creatures you control" — compound subtype+type phrase.
        // parse_type_phrase handles "or" between subtypes when the second branch includes
        // a core type ("rogue creatures"), producing an Or filter.
        let (filter, remainder) = parse_type_phrase("ninja or rogue creatures you control");
        assert!(
            remainder.trim().is_empty(),
            "remainder should be empty, got: '{remainder}'"
        );
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(filters.len(), 2, "expected 2 Or branches, got {filters:?}");
        } else {
            panic!("expected Or filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_comma_or_three_types() {
        // CR 205.3a: "artifact, creature, or enchantment" — all 3 must appear in Or
        let (filter, rest) = parse_type_phrase("artifact, creature, or enchantment");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(
                filters.len(),
                3,
                "expected 3 Or branches, got {}",
                filters.len()
            );
        } else {
            panic!("Expected Or filter");
        }
    }

    #[test]
    fn parse_type_phrase_comma_or_with_controller() {
        // "artifact, creature, or enchantment you control" — controller distributes
        let (filter, rest) = parse_type_phrase("artifact, creature, or enchantment you control");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(filters.len(), 3);
            for f in filters {
                if let TargetFilter::Typed(tf) = f {
                    assert_eq!(
                        tf.controller,
                        Some(ControllerRef::You),
                        "controller missing on {:?}",
                        tf.get_primary_type()
                    );
                } else {
                    panic!("Expected Typed in Or");
                }
            }
        } else {
            panic!("Expected Or filter");
        }
    }

    #[test]
    fn parse_type_phrase_aura_card_stays_generic() {
        let (filter, rest) =
            parse_type_phrase("Aura card with mana value less than or equal to that Aura");
        assert_eq!(rest.trim(), "Aura", "remainder: '{rest}'");
        let TargetFilter::Typed(typed) = filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert_eq!(typed.get_subtype(), Some("Aura"));
        assert!(
            typed
                .type_filters
                .iter()
                .position(|type_filter| *type_filter == TypeFilter::Enchantment)
                .is_none(),
            "search-only normalization should not happen in parse_type_phrase: {typed:?}"
        );
        assert!(typed.properties.iter().any(|property| matches!(
            property,
            FilterProp::CmcLE {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextSourceManaValue
                }
            }
        )));
    }

    #[test]
    fn combat_status_prefix_unblocked() {
        let result = parse_combat_status_prefix("unblocked attacking creatures");
        assert_eq!(result, Some((FilterProp::Unblocked, 10)));
        // Second call on remainder should get Attacking
        let result2 = parse_combat_status_prefix("attacking creatures");
        assert_eq!(result2, Some((FilterProp::Attacking, 10)));
    }

    #[test]
    fn parse_type_phrase_unblocked_attacking_creatures_you_control() {
        let (filter, remainder) = parse_type_phrase("unblocked attacking creatures you control");
        assert!(remainder.trim().is_empty(), "remainder: '{remainder}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.properties.contains(&FilterProp::Unblocked));
            assert!(tf.properties.contains(&FilterProp::Attacking));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_tapped_creature() {
        let (filter, rest) = parse_type_phrase("tapped creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf.properties.contains(&FilterProp::Tapped));
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_untapped_land() {
        let (filter, rest) = parse_type_phrase("untapped land");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
            assert!(tf.properties.contains(&FilterProp::Untapped));
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_tapped_artifact_or_creature() {
        // "tapped artifact or creature" — tapped is a leading prefix, applied to the left branch.
        // The "or" handler applies right→left property distribution only, so tapped stays
        // on the artifact branch. (Full leading-property distribution is a separate concern.)
        let (filter, rest) = parse_type_phrase("tapped artifact or creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(filters.len(), 2);
            // Left branch: Artifact with Tapped
            if let TargetFilter::Typed(tf) = &filters[0] {
                assert!(tf.type_filters.contains(&TypeFilter::Artifact));
                assert!(tf.properties.contains(&FilterProp::Tapped));
            } else {
                panic!("Expected Typed, got {:?}", filters[0]);
            }
            // Right branch: Creature (no Tapped — not distributed from left)
            if let TargetFilter::Typed(tf) = &filters[1] {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            } else {
                panic!("Expected Typed, got {:?}", filters[1]);
            }
        } else {
            panic!("Expected Or filter, got {filter:?}");
        }
    }

    #[test]
    fn that_share_creature_type_consumed() {
        // CR 700.5: "that share a creature type" is consumed into SharesQuality.
        let (filter, rest) = parse_type_phrase("creatures you control that share a creature type");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.iter().any(
                |p| matches!(p, FilterProp::SharesQuality { quality } if *quality == SharedQuality::CreatureType)
            ));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_share_no_creature_types_consumed() {
        let (filter, rest) = parse_type_phrase("creatures that share no creature types");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::SharesQuality { quality } if *quality == SharedQuality::CreatureType)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn target_that_share_full_parse() {
        let (filter, rest) =
            parse_target("target creatures you control that share a creature type");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::SharesQuality { .. })));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_was_dealt_damage_this_turn() {
        let (filter, rest) = parse_target("target creature that was dealt damage this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::WasDealtDamageThisTurn)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_was_dealt_damage_with_controller() {
        let (filter, rest) =
            parse_target("target creature an opponent controls that was dealt damage this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::WasDealtDamageThisTurn)),
                "Expected WasDealtDamageThisTurn in properties: {:?}",
                tf.properties
            );
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_entered_this_turn() {
        let (filter, rest) = parse_type_phrase("token you control that entered this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.iter().any(|p| matches!(p, FilterProp::Token)));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::EnteredThisTurn)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_entered_the_battlefield_this_turn() {
        let (filter, rest) = parse_type_phrase("creature that entered the battlefield this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::EnteredThisTurn)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_attacked_this_turn() {
        let (filter, rest) = parse_target("target creature that attacked this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::AttackedThisTurn)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_blocked_this_turn() {
        let (filter, rest) = parse_target("target creature that blocked this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::BlockedThisTurn)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    #[test]
    fn that_attacked_or_blocked_this_turn() {
        let (filter, rest) = parse_target("target creature that attacked or blocked this turn");
        if let TargetFilter::Typed(ref tf) = filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::AttackedOrBlockedThisTurn)));
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
        assert!(
            rest.trim().is_empty(),
            "expected empty remainder, got: {rest:?}"
        );
    }

    // --- CR 303.4 + CR 301.5: "that's enchanted or equipped" relative-clause tests ---
    // Compound-subject grant class (Reyav, Master Smith; Dogmeat, Ever Loyal).

    #[test]
    fn that_s_enchanted_or_equipped_emits_disjunction() {
        let result = parse_that_clause_suffix(" that's enchanted or equipped");
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        match &props[0] {
            FilterProp::HasAnyAttachmentOf { kinds, controller } => {
                assert_eq!(
                    kinds,
                    &vec![AttachmentKind::Aura, AttachmentKind::Equipment]
                );
                assert_eq!(controller, &None);
            }
            other => panic!("expected HasAnyAttachmentOf, got {other:?}"),
        }
    }

    #[test]
    fn that_s_equipped_or_enchanted_emits_disjunction() {
        let result = parse_that_clause_suffix(" that's equipped or enchanted");
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::HasAnyAttachmentOf { kinds, .. }
                if kinds.len() == 2 && kinds.contains(&AttachmentKind::Aura)
                    && kinds.contains(&AttachmentKind::Equipment)
        ));
    }

    #[test]
    fn that_s_enchanted_only_emits_single_kind() {
        let result = parse_that_clause_suffix(" that's enchanted");
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::HasAttachment {
                kind: AttachmentKind::Aura,
                controller: None,
            }
        ));
    }

    #[test]
    fn that_s_equipped_only_emits_single_kind() {
        let result = parse_that_clause_suffix(" that's equipped");
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::HasAttachment {
                kind: AttachmentKind::Equipment,
                controller: None,
            }
        ));
    }

    #[test]
    fn that_s_enchanted_or_equipped_in_full_target() {
        // Reyav / Dogmeat trigger subject form.
        let (filter, _rest) = parse_target("a creature you control that's enchanted or equipped");
        match filter {
            TargetFilter::Typed(tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::HasAnyAttachmentOf { kinds, .. } if kinds.len() == 2
                )));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    // --- CR 115.9c: "that targets only [X]" tests ---

    #[test]
    fn that_targets_only_self_ref() {
        let result = parse_that_clause_suffix(" that targets only ~");
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::TargetsOnly { filter } if **filter == TargetFilter::SelfRef
        ));
    }

    #[test]
    fn that_targets_only_it() {
        let result = parse_that_clause_suffix(" that targets only it,");
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::TargetsOnly { filter } if **filter == TargetFilter::SelfRef
        ));
        // Should consume up to "it" but not the comma
        assert_eq!(consumed, " that targets only it".len());
    }

    #[test]
    fn that_targets_only_you() {
        let result = parse_that_clause_suffix(" that targets only you,");
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::TargetsOnly { filter }
                if matches!(&**filter, TargetFilter::Typed(TypedFilter { controller: Some(ControllerRef::You), .. }))
        ));
        assert_eq!(consumed, " that targets only you".len());
    }

    #[test]
    fn that_targets_only_single_creature_you_control() {
        let result = parse_that_clause_suffix(" that targets only a single creature you control,");
        let (props, consumed) = result.expect("should parse");
        // Should produce TargetsOnly + HasSingleTarget
        assert_eq!(props.len(), 2);
        assert!(matches!(&props[0], FilterProp::TargetsOnly { .. }));
        assert!(matches!(&props[1], FilterProp::HasSingleTarget));
        if let FilterProp::TargetsOnly { filter } = &props[0] {
            if let TargetFilter::Typed(tf) = &**filter {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            } else {
                panic!("expected Typed inner filter, got {filter:?}");
            }
        }
        assert_eq!(
            consumed,
            " that targets only a single creature you control".len()
        );
    }

    #[test]
    fn that_targets_only_single_permanent_or_player() {
        let result = parse_that_clause_suffix(" that targets only a single permanent or player");
        let (props, _consumed) = result.expect("should parse");
        assert_eq!(props.len(), 2);
        assert!(matches!(&props[0], FilterProp::TargetsOnly { .. }));
        assert!(matches!(&props[1], FilterProp::HasSingleTarget));
        if let FilterProp::TargetsOnly { filter } = &props[0] {
            assert!(
                matches!(&**filter, TargetFilter::Or { .. }),
                "expected Or filter for 'permanent or player', got {filter:?}"
            );
        }
    }

    #[test]
    fn type_phrase_with_targets_only_self() {
        // "instant or sorcery spell that targets only ~"
        let (filter, rest) =
            parse_type_phrase("instant or sorcery spell that targets only ~, copy");
        assert_eq!(rest.trim_start().trim_start_matches(',').trim(), "copy");
        // The filter should be Or(Instant + TargetsOnly, Sorcery + TargetsOnly)
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(filters.len(), 2);
            for f in filters {
                if let TargetFilter::Typed(tf) = f {
                    assert!(
                        tf.properties
                            .iter()
                            .any(|p| matches!(p, FilterProp::TargetsOnly { .. })),
                        "expected TargetsOnly in properties of {tf:?}"
                    );
                } else {
                    panic!("expected Typed filter in Or, got {f:?}");
                }
            }
        } else {
            panic!("expected Or filter, got {filter:?}");
        }
    }

    // --- CR 115.9b: "that targets [X]" tests (.any() semantics) ---

    #[test]
    fn that_targets_self_ref() {
        let result = parse_that_clause_suffix(" that targets this creature,");
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::Targets { filter } if **filter == TargetFilter::SelfRef
        ));
        assert_eq!(consumed, " that targets this creature".len());
    }

    #[test]
    fn that_targets_tilde() {
        let result = parse_that_clause_suffix(" that targets ~,");
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::Targets { filter } if **filter == TargetFilter::SelfRef
        ));
        assert_eq!(consumed, " that targets ~".len());
    }

    #[test]
    fn that_targets_this_permanent() {
        let result = parse_that_clause_suffix(" that targets this permanent,");
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::Targets { filter } if **filter == TargetFilter::SelfRef
        ));
        assert_eq!(consumed, " that targets this permanent".len());
    }

    #[test]
    fn that_targets_you() {
        let result = parse_that_clause_suffix(" that targets you,");
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        assert!(matches!(
            &props[0],
            FilterProp::Targets { filter } if **filter == TargetFilter::Controller
        ));
        assert_eq!(consumed, " that targets you".len());
    }

    #[test]
    fn that_targets_you_or_a_creature() {
        let result = parse_that_clause_suffix(" that targets you or a creature you control,");
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        if let FilterProp::Targets { filter } = &props[0] {
            if let TargetFilter::Or { filters } = &**filter {
                assert_eq!(filters.len(), 2);
                assert_eq!(filters[0], TargetFilter::Controller);
                if let TargetFilter::Typed(tf) = &filters[1] {
                    assert!(tf.type_filters.contains(&TypeFilter::Creature));
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                } else {
                    panic!("expected Typed filter, got {:?}", filters[1]);
                }
            } else {
                panic!("expected Or filter, got {filter:?}");
            }
        } else {
            panic!("expected Targets prop, got {:?}", props[0]);
        }
        assert_eq!(
            consumed,
            " that targets you or a creature you control".len()
        );
    }

    #[test]
    fn that_targets_one_or_more_creatures() {
        // "one or more" prefix is stripped (redundant with .any() semantics)
        let result = parse_that_clause_suffix(" that targets one or more creatures you control,");
        let (props, consumed) = result.expect("should parse");
        assert_eq!(props.len(), 1);
        if let FilterProp::Targets { filter } = &props[0] {
            if let TargetFilter::Typed(tf) = &**filter {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            } else {
                panic!("expected Typed filter, got {filter:?}");
            }
        } else {
            panic!("expected Targets prop, got {:?}", props[0]);
        }
        assert_eq!(
            consumed,
            " that targets one or more creatures you control".len()
        );
    }

    #[test]
    fn type_phrase_spell_that_targets_self() {
        // "spell that targets this creature" via parse_type_phrase
        let (filter, rest) = parse_type_phrase("spell that targets this creature, put");
        assert_eq!(rest.trim_start().trim_start_matches(',').trim(), "put");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Card));
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::Targets { filter } if **filter == TargetFilter::SelfRef)),
                "expected Targets {{ SelfRef }} in properties: {:?}",
                tf.properties
            );
        } else {
            panic!("expected Typed filter, got {filter:?}");
        }
    }

    // ── VERB-01: Compound target type patterns ──

    #[test]
    fn parse_type_phrase_creature_or_planeswalker() {
        let (filter, rest) = parse_type_phrase("creature or planeswalker");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(filters.len(), 2);
            assert_eq!(filters[0], TargetFilter::Typed(TypedFilter::creature()));
            assert_eq!(
                filters[1],
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker))
            );
        } else {
            panic!("Expected Or filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_nonland_permanent() {
        let (filter, rest) = parse_type_phrase("nonland permanent");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Permanent));
            assert!(tf
                .type_filters
                .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_creature_with_power_3_or_greater() {
        let (filter, rest) = parse_type_phrase("creature with power 3 or greater");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(
                tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::PowerGE {
                        value: QuantityExpr::Fixed { value: 3 }
                    }
                )),
                "Expected PowerGE(3) in {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_creature_with_greater_power() {
        // CR 509.1b: "creatures with greater power" — relative to source
        let (filter, rest) = parse_type_phrase("creatures with greater power");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::PowerGTSource)),
                "Expected PowerGTSource in {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_creature_without_flying() {
        let (filter, rest) = parse_type_phrase("creature without flying");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(
                tf.properties.iter().any(
                    |p| matches!(p, FilterProp::WithoutKeyword { value } if *value == Keyword::Flying)
                ),
                "Expected WithoutKeyword(Flying) in {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_creature_without_first_strike() {
        let (filter, rest) = parse_type_phrase("creature without first strike");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(
                tf.properties.iter().any(
                    |p| matches!(p, FilterProp::WithoutKeyword { value } if *value == Keyword::FirstStrike)
                ),
                "Expected WithoutKeyword(FirstStrike) in {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_another_creature() {
        let (filter, rest) = parse_type_phrase("another creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(
                tf.properties.contains(&FilterProp::Another),
                "Expected Another property in {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_type_phrase_another_creature_you_control() {
        let (filter, rest) = parse_type_phrase("another creature you control");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf.properties.contains(&FilterProp::Another));
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_target_another_target_creature() {
        // "another target creature" via parse_target: "target " prefix consumed,
        // then parse_type_phrase("another creature") should add Another property.
        let (filter, rest) = parse_target("target another creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(
                tf.properties.contains(&FilterProp::Another),
                "Expected Another property in {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {filter:?}");
        }
    }

    #[test]
    fn parse_target_other_target_creature_or_spell() {
        let (filter, rest) = parse_target("other target creature or spell");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = filter else {
            panic!("Expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
        assert!(filters.iter().any(|filter| matches!(
            filter,
            TargetFilter::Typed(tf)
                if tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.properties.contains(&FilterProp::Another)
        )));
        assert!(filters.iter().any(|filter| matches!(
            filter,
            TargetFilter::Typed(tf)
                if tf.type_filters.contains(&TypeFilter::Card)
                    && tf.properties.contains(&FilterProp::Another)
        )));
    }

    #[test]
    fn parse_type_phrase_artifact_creature_or_enchantment() {
        // 3-way Or: "artifact, creature, or enchantment"
        let (filter, rest) = parse_type_phrase("artifact, creature, or enchantment");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        if let TargetFilter::Or { filters } = &filter {
            assert_eq!(
                filters.len(),
                3,
                "expected 3 branches, got {}",
                filters.len()
            );
            // Verify each branch has the correct type
            let types: Vec<_> = filters
                .iter()
                .filter_map(|f| {
                    if let TargetFilter::Typed(tf) = f {
                        tf.get_primary_type()
                    } else {
                        None
                    }
                })
                .collect();
            assert!(types.contains(&&TypeFilter::Artifact));
            assert!(types.contains(&&TypeFilter::Creature));
            assert!(types.contains(&&TypeFilter::Enchantment));
        } else {
            panic!("Expected Or filter, got {filter:?}");
        }
    }

    /// CR 205.2a: "artifact creature" is the conjunction of two core card types,
    /// not a sole Artifact filter. Regression for Lux Artillery: "whenever you
    /// cast an artifact creature spell" previously dropped the Creature type.
    #[test]
    fn parse_type_phrase_artifact_creature_conjunction() {
        let (filter, rest) = parse_type_phrase("artifact creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(
            tf.type_filters.contains(&TypeFilter::Artifact),
            "expected Artifact in {:?}",
            tf.type_filters
        );
        assert!(
            tf.type_filters.contains(&TypeFilter::Creature),
            "expected Creature in {:?}",
            tf.type_filters
        );
    }

    /// CR 205.2a + CR 601.2: "artifact creature spell" — the trailing "spell"
    /// suffix is informational and should be stripped after the conjunction.
    #[test]
    fn parse_type_phrase_artifact_creature_spell() {
        let (filter, rest) = parse_type_phrase("artifact creature spell");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Artifact));
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
    }

    /// CR 205.2a + CR 205.4b: "noncreature artifact" — negation followed by a
    /// concrete core type. The Non(Creature) negation should land in
    /// type_filters alongside Artifact.
    #[test]
    fn parse_type_phrase_noncreature_artifact() {
        let (filter, rest) = parse_type_phrase("noncreature artifact");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Artifact));
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Non(Box::new(TypeFilter::Creature))),
            "expected Non(Creature) in {:?}",
            tf.type_filters
        );
    }

    /// CR 205.4a: "legendary creature" — legendary is a supertype, not a core
    /// type. Must remain a single-type filter with a HasSupertype property.
    #[test]
    fn parse_type_phrase_legendary_creature_keeps_supertype_prop() {
        let (filter, rest) = parse_type_phrase("legendary creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(
            tf.properties.iter().any(|prop| matches!(
                prop,
                FilterProp::HasSupertype {
                    value: Supertype::Legendary
                }
            )),
            "expected HasSupertype(Legendary) in {:?}",
            tf.properties
        );
    }

    /// CR 205.2a: "artifact or creature" is an OR-union of the two core types,
    /// NOT a conjunction. The separator " or " breaks out of the conjunction
    /// loop and builds a TargetFilter::Or with two branches.
    #[test]
    fn parse_type_phrase_artifact_or_creature_stays_union() {
        let (filter, rest) = parse_type_phrase("artifact or creature");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Or { filters } = &filter else {
            panic!("Expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 2);
    }

    /// CR 205.2a + CR 110.1: "artifact creature you control" — conjunction
    /// followed by a controller suffix.
    #[test]
    fn parse_type_phrase_artifact_creature_you_control() {
        let (filter, rest) = parse_type_phrase("artifact creature you control");
        assert!(rest.trim().is_empty(), "remainder: '{rest}'");
        let TargetFilter::Typed(tf) = &filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Artifact));
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert_eq!(tf.controller, Some(ControllerRef::You));
    }

    #[test]
    fn parse_target_bare_possessive_graveyard() {
        let (f, rest) = parse_target("their graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::InZone {
                    zone: Zone::Graveyard
                }],
                ..Default::default()
            })
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_bare_possessive_library() {
        let (f, rest) = parse_target("your library");
        assert_eq!(
            f,
            TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::InZone {
                    zone: Zone::Library
                }],
                ..Default::default()
            })
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_opponents_graveyard() {
        let (filter, rest) = parse_target("opponent's graveyard");
        assert_eq!(
            filter,
            TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::Owned {
                    controller: ControllerRef::Opponent,
                },
                FilterProp::InZone {
                    zone: Zone::Graveyard,
                },
            ]))
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn target_card_from_an_opponents_graveyard() {
        // Lord Skitter, Sewer King: "exile up to one target card from an opponent's graveyard"
        // Uses Owned{Opponent} (checks obj.owner) so stolen creatures that died and went to
        // their owner's graveyard are correctly included per CR 404.2.
        let (filter, rest) = parse_target("target card from an opponent's graveyard");
        assert_eq!(
            filter,
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Card],
                controller: None,
                properties: vec![
                    FilterProp::Owned {
                        controller: ControllerRef::Opponent,
                    },
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ],
            })
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn scan_zone_phrase_finds_trailing_zone_after_subject() {
        // "this card is in your graveyard" — scanner must skip "this card is" and
        // find the zone phrase at a later word boundary.
        let (zone, ctrl, _props) = scan_zone_phrase("this card is in your graveyard").unwrap();
        assert_eq!(zone, Zone::Graveyard);
        assert_eq!(ctrl, Some(ControllerRef::You));
    }

    #[test]
    fn scan_zone_phrase_finds_exile_and_hand() {
        // Delegation from oracle_condition now picks up non-graveyard zones, which
        // SourceInZone supports uniformly — lock in that behavior.
        assert_eq!(
            scan_zone_phrase("~ in exile").map(|(z, _, _)| z),
            Some(Zone::Exile)
        );
        assert_eq!(
            scan_zone_phrase("this card from your hand").map(|(z, _, _)| z),
            Some(Zone::Hand)
        );
    }

    #[test]
    fn scan_zone_phrase_returns_none_without_zone() {
        assert!(scan_zone_phrase("this creature is attacking").is_none());
        assert!(scan_zone_phrase("you control a legendary creature").is_none());
        // Word-boundary safety: "graveyardkeeper" must not match as "graveyard".
        assert!(scan_zone_phrase("from your graveyardkeeper").is_none());
    }

    #[test]
    fn target_card_from_each_opponents_graveyard() {
        // Regression: "each opponent's" is in POSSESSIVES, so without the dedicated
        // opponent branch it would fall through to the generic possessive arm with
        // no ownership constraint. Mirrors the "an opponent's" case per CR 404.2.
        let (filter, rest) = parse_target("target card from each opponent's graveyard");
        assert_eq!(
            filter,
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Card],
                controller: None,
                properties: vec![
                    FilterProp::Owned {
                        controller: ControllerRef::Opponent,
                    },
                    FilterProp::InZone {
                        zone: Zone::Graveyard,
                    },
                ],
            })
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_target_the_creatures_controller() {
        let (filter, rest) = parse_target("the creature's controller");
        assert_eq!(filter, TargetFilter::ParentTargetController);
        assert_eq!(rest, "");
    }

    /// CR 205.3 + CR 205.4b: "target attacking Vampire that isn't a Demon" — the
    /// subtype-negation relative clause must append `Non(Subtype("Demon"))` to
    /// the target's type filters so a Vampire Demon is rejected.
    #[test]
    fn parse_target_that_isnt_subtype_appends_negation() {
        let (filter, _) = parse_target("target attacking Vampire that isn't a Demon");
        match filter {
            TargetFilter::Typed(tf) => {
                assert!(
                    tf.type_filters
                        .contains(&TypeFilter::Subtype("Vampire".into())),
                    "expected Vampire subtype in type_filters, got {:?}",
                    tf.type_filters
                );
                assert!(
                    tf.type_filters
                        .contains(&TypeFilter::Non(Box::new(TypeFilter::Subtype(
                            "Demon".into()
                        )))),
                    "expected Non(Subtype(Demon)) in type_filters, got {:?}",
                    tf.type_filters
                );
                assert!(
                    tf.properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::Attacking)),
                    "expected Attacking property, got {:?}",
                    tf.properties
                );
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    /// CR 205.3: "that's not a <Subtype>" — contraction form.
    #[test]
    fn parse_target_thats_not_subtype_appends_negation() {
        let (filter, _) = parse_target("target Vampire that's not a Demon");
        match filter {
            TargetFilter::Typed(tf) => assert!(
                tf.type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Subtype(
                        "Demon".into()
                    )))),
                "expected Non(Subtype(Demon)) in type_filters, got {:?}",
                tf.type_filters
            ),
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    /// CR 205.3: "that is not <Subtype>" — unabridged variant without article.
    #[test]
    fn parse_target_that_is_not_subtype_appends_negation() {
        let (filter, _) = parse_target("target creature that is not Human");
        match filter {
            TargetFilter::Typed(tf) => assert!(
                tf.type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Subtype(
                        "Human".into()
                    )))),
                "expected Non(Subtype(Human)) in type_filters, got {:?}",
                tf.type_filters
            ),
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }
}
