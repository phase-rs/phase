use std::str::FromStr;

use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::{all_consuming, opt, rest, value};
use nom::Parser;

use crate::parser::oracle_ir::context::{ParseContext, TokenPtFollowup};
use crate::parser::oracle_nom::error::OracleResult;
use crate::types::ability::{
    ContinuousModification, ControllerRef, Effect, FilterProp, ObjectScope, PtValue, QuantityExpr,
    QuantityRef, StaticDefinition, TargetFilter, TypeFilter,
};
use crate::types::card_type::Supertype;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_static::{parse_quoted_ability_modifications, parse_static_line_multi};
use super::super::oracle_target::{parse_target, parse_target_with_ctx};
use super::super::oracle_util::{
    comma_short_self_name, normalize_card_name_refs, parse_count_expr, parse_rounding_suffix_only,
    rewrite_quantity_expr_rounding, strip_reminder_text, TextPair,
};
use crate::parser::oracle_ir::ast::*;

/// Bridge: run a nom combinator on a lowercase copy, mapping the consumed length
/// back to the original-case text to compute the correct remainder.
fn nom_on_lower<'a, T, F>(text: &'a str, lower: &str, mut parser: F) -> Option<(T, &'a str)>
where
    F: FnMut(&str) -> OracleResult<'_, T>,
{
    let (rest, result) = parser(lower).ok()?;
    let consumed = lower.len() - rest.len();
    Some((result, &text[consumed..]))
}

pub(crate) fn try_parse_token(_lower: &str, text: &str, ctx: &mut ParseContext) -> Option<Effect> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();

    // "create a token that's a copy of {target}"
    if let Ok((_, (tapped, enters_attacking, mut count))) = parse_copy_token_entry_modifiers(&lower)
    {
        let tp = TextPair::new(&text, &lower);
        let after_copy_tp = tp
            .strip_after("copy of ")
            .or_else(|| tp.strip_after("copies of "))
            .unwrap_or(tp);
        // Handle "another target ..." -- strip "another" prefix and add FilterProp::Another
        let has_another = nom_on_lower(after_copy_tp.original, after_copy_tp.lower, |i| {
            value((), tag("another ")).parse(i)
        })
        .is_some();
        let target_text = if has_another {
            after_copy_tp.strip_prefix("another ").unwrap().original
        } else {
            after_copy_tp.original
        };
        // CR 707.2 + CR 707.9: "…copy of {target}, except <body>" — strip the
        // optional except clause before target parsing so the trailing
        // modification phrase doesn't pollute the target filter. The except
        // body may produce both keyword grants (`extra_keywords`) and
        // non-keyword modifications such as `RemoveSupertype` for Miirym's
        // "except the token isn't legendary" — both are channelled through
        // the shared `parse_except_clause` building block. `card_name` is
        // empty here because the copy source is unknown at parse time;
        // `SetName` arms in the except clause decline gracefully when
        // `card_name` is empty (see `become_copy_except.rs::parse_name_override`).
        let (target_text, extra_keywords, additional_modifications) =
            split_token_except_clause(target_text, ctx);
        let target_lower = target_text.trim().to_lowercase();
        let (mut target, _) = if parse_cost_paid_object_copy_target(&target_lower) {
            (TargetFilter::CostPaidObject, "")
        } else {
            parse_target_with_ctx(target_text, ctx)
        };
        if has_another {
            if let TargetFilter::Typed(ref mut typed) = target {
                if !typed.properties.contains(&FilterProp::Another) {
                    typed.properties.push(FilterProp::Another);
                }
            }
        }
        // CR 303.4 + CR 702.103: Inside an Aura/bestow card, a `"that creature"`
        // anaphor in the copy-token clause is the antecedent of the attachment
        // host ("a creature you control") in the enclosing condition — not a
        // chosen target. The generic `parse_target` family returns
        // `TargetFilter::ParentTarget` for "that creature" because attachment
        // context is not threaded through the effect parser. When the parse
        // context exposes a typed host self-reference (`host_self_reference`,
        // set by `parse_oracle_ir` only for Aura/bestow cards), remap a
        // `ParentTarget` copy target to the host filter so the runtime resolves
        // the copy against the enchanted creature. Non-Aura cards leave
        // `host_self_reference` `None`, so `ParentTarget` keeps its
        // chosen-target meaning (Twinflame Strike's "for each of them").
        if let (TargetFilter::ParentTarget, Some(host)) = (&target, &ctx.host_self_reference) {
            target = host.clone();
        }
        // CR 107.3: bind a variable "X" count to its "where X is <quantity>"
        // clause (Devastating Onslaught, Nacatl War-Pride, Rionya), mirroring the
        // non-copy token path. A bare X with no where-clause (Aggressive Biomancy)
        // is left as `Variable("X")` for the spell's X cost to resolve.
        if matches!(&count, QuantityExpr::Ref { qty: QuantityRef::Variable { ref name } } if name == "X")
        {
            if let Some(where_expression) = extract_token_where_x_expression(&text) {
                // CR 107.3c: the clause DEFINES X. If the definition is not
                // representable, this copy-token clause does not lower — fail the
                // parse instead of fabricating a raw-text placeholder. The
                // fabricated `QuantityRef::Variable { name: "<oracle text>" }` is
                // well-typed but DEAD (game/quantity.rs resolves a non-`X` variable
                // name to 0), so the effect copied ZERO tokens while the raw text
                // still rendered as a supported dynamic quantity. This mirrors the
                // sibling non-copy token path below.
                count =
                    super::parse_where_x_quantity_expression(&where_expression).or_else(|| {
                        crate::parser::oracle_quantity::parse_cda_quantity(&where_expression)
                    })?;
            }
        }
        return Some(Effect::CopyTokenOf {
            target,
            // CR 109.4: Default to the controller; a "target [player] creates"
            // subject is lifted into `owner` later by `inject_subject_target`.
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking,
            tapped,
            count,
            extra_keywords,
            additional_modifications,
        });
    }

    let after = nom_on_lower(&text, &lower, |i| value((), tag("create ")).parse(i))
        .map(|(_, rest)| rest)
        .unwrap_or(&text)
        .trim();
    let token = parse_token_description_with_context(after, ctx)?;
    Some(Effect::Token {
        name: token.name,
        power: token.power.unwrap_or(PtValue::Fixed(0)),
        toughness: token.toughness.unwrap_or(PtValue::Fixed(0)),
        types: token.types,
        colors: token.colors,
        keywords: token.keywords,
        tapped: token.tapped,
        count: token.count,
        owner: TargetFilter::Controller,
        attach_to: token.attach_to,
        enters_attacking: token.enters_attacking,
        // CR 205.4a: Carry parsed supertypes (e.g. "legendary" for Marit Lage)
        // onto the token so the legend rule (CR 704.5j) applies.
        supertypes: token.supertypes,
        static_abilities: token.static_abilities,
        enter_with_counters: vec![],
    })
}

pub(super) fn parse_copy_token_entry_modifiers(
    input: &str,
) -> OracleResult<'_, (bool, bool, QuantityExpr)> {
    let (rest, _) = tag("create ").parse(input)?;
    // The bare article "a"/"one" → a count of 1. `parse_count_expr` intentionally
    // excludes the article (to avoid matching the "a" in "another"), so handle it
    // here; otherwise delegate to the shared count grammar so "X", "two", "twice
    // X", "that many", etc. all parse uniformly — mirroring the non-copy token
    // path's `parse_token_count_prefix`. Without this, "Create X tokens that are
    // copies of …" failed to parse and the whole effect was dropped.
    let (rest, count) =
        if let Ok((rest, _)) = alt((tag::<_, _, OracleError<'_>>("a "), tag("one "))).parse(rest) {
            (rest, Some(QuantityExpr::Fixed { value: 1 }))
        } else if let Some((expr, rest_after)) = parse_count_expr(rest) {
            (rest_after, Some(expr))
        } else {
            (rest, None)
        };
    let (rest, _) = if count.is_some() {
        opt(tag(" ")).parse(rest)?
    } else {
        (rest, None)
    };
    let (rest, flags) = alt((
        value((true, true), tag("tapped and attacking ")),
        value((true, false), tag("tapped ")),
        value((false, true), tag("attacking ")),
        value((false, false), tag("")),
    ))
    .parse(rest)?;
    let (rest, _) = alt((
        tag("token that's a copy of"),
        tag("token thats a copy of"),
        tag("tokens that are copies of"),
    ))
    .parse(rest)?;
    Ok((
        rest,
        (
            flags.0,
            flags.1,
            count.unwrap_or(QuantityExpr::Fixed { value: 1 }),
        ),
    ))
}

fn parse_cost_paid_object_copy_target(lower: &str) -> bool {
    matches!(
        lower.trim_end_matches('.'),
        "the exiled card" | "the card exiled this way"
    )
}

/// CR 707.2 + CR 707.9: Split off a trailing `[, ]except <body>` clause from a
/// copy-of-target phrase, channeling both keyword grants and non-keyword
/// modifications through the shared `parse_except_clause` building block.
///
/// Returns `(target_text_without_clause, extra_keywords, additional_modifications)`.
///
/// The keyword list is extracted from the modifications by filtering out
/// `ContinuousModification::AddKeyword` variants — `Effect::CopyTokenOf` keeps
/// `extra_keywords: Vec<Keyword>` as a typed convenience for the keyword case
/// (Twinflame), and the rest of the modifications populate
/// `additional_modifications: Vec<ContinuousModification>` (Miirym's
/// `RemoveSupertype`, conditional counter additions, etc.).
///
/// Example: `"that creature, except it has haste"` →
///   (`"that creature"`, `vec![Keyword::Haste]`, `vec![]`)
///
/// Example: `"it, except the token isn't legendary"` →
///   (`"it"`, `vec![]`, `vec![RemoveSupertype { Legendary }]`)
fn split_token_except_clause<'a>(
    text: &'a str,
    ctx: &ParseContext,
) -> (&'a str, Vec<Keyword>, Vec<ContinuousModification>) {
    let lower = text.to_lowercase();
    let Ok((_, head_lower)) = parse_token_except_boundary(&lower) else {
        return (text, Vec::new(), Vec::new());
    };
    let head = &text[..head_lower.len()];
    // CR 707.9b + CR 707.2: a token-copy exception can rename the copy with a
    // literal name ("…named Mishra's Warform…", Mishra, Eminent One). Unlike the
    // self-name "its name is ~" arm — which keys off the copying card's own name
    // and so cannot apply to a token copy (`card_name` empty below) — a literal
    // override carries the name in the text itself, so peel it off here (original
    // case preserved) and strip the "named <X>" span before the body reaches the
    // shared except parser. Without this the name words leak into the copied
    // creature's subtype list AND the override is dropped, so a token copying a
    // legendary permanent keeps the source's name and wrongly collides with it
    // under the legend rule (CR 704.5j). The original-case except body is
    // byte-aligned to its lowercase form (mirrors the `head` slice above).
    let except_original = &text[head_lower.len()..];
    let (name_is_override, except_body) =
        strip_copy_except_name_is_override(except_original, ctx.card_name.as_deref());
    let (named_override, except_body) = if name_is_override.is_none() {
        strip_copy_except_named_override(&except_body)
    } else {
        (None, except_body)
    };
    let name_override = name_is_override.or(named_override);
    let except_lower = except_body.to_lowercase();

    // Pass the lowercase suffix starting at `[, ]except ` to the shared
    // building block. The except parser is the single authority for the
    // grammar (CR 707.9 + CR 707.2): keyword lists, supertype additions /
    // removals, conditional counter placement, etc.
    let card_name = ""; // SetName cannot apply to token-copy (source unknown at parse time).
    let mut extra_keywords = Vec::new();
    let mut additional_modifications = Vec::new();
    match super::become_copy_except::parse_except_clause(&except_lower, card_name, ctx) {
        Some((_, modifications)) => {
            for modification in modifications {
                match modification {
                    ContinuousModification::AddKeyword { keyword } => extra_keywords.push(keyword),
                    other => additional_modifications.push(other),
                }
            }
        }
        // A clause that is *only* a literal name override (no other recognised
        // body) still yields the rename — don't discard it.
        None if name_override.is_none() => return (head, Vec::new(), Vec::new()),
        None => {}
    }

    if let Some(name) = name_override {
        additional_modifications.push(ContinuousModification::SetName { name });
    }
    (head, extra_keywords, additional_modifications)
}

/// CR 201.5c + CR 707.9b: peel a literal `"<possessive> name is <X>"`
/// rename off a token-copy `, except <body>` clause while preserving the
/// following exception body for the shared copy-except parser.
fn strip_copy_except_name_is_override(
    body: &str,
    card_name: Option<&str>,
) -> (Option<String>, String) {
    let lower = body.to_lowercase();
    let Some((possessive, after_prefix)) = nom_on_lower(body, &lower, |i| {
        let (i, _) = alt((tag::<_, _, OracleError<'_>>(", except "), tag(" except "))).parse(i)?;
        super::become_copy_except::parse_copy_name_is_prefix(i)
    }) else {
        return (None, body.to_string());
    };
    let after_prefix_lower = after_prefix.to_lowercase();
    let Some((name_text, continuation)) =
        split_copy_name_body(after_prefix, &after_prefix_lower, possessive)
    else {
        return (None, body.to_string());
    };
    let name_override = reconstruct_copy_exception_name(name_text, card_name);
    let stripped = format!("{}{}", copy_except_prefix(body, &lower), continuation);
    (name_override, stripped)
}

fn copy_except_prefix<'a>(body: &'a str, lower: &str) -> &'a str {
    let Some((_, rest)) = nom_on_lower(body, lower, |i| {
        value(
            (),
            alt((tag::<_, _, OracleError<'_>>(", except "), tag(" except "))),
        )
        .parse(i)
    }) else {
        return "";
    };
    let prefix_len = body.len() - rest.len();
    &body[..prefix_len]
}

fn split_copy_name_body<'a>(
    original: &'a str,
    lower: &str,
    possessive: super::become_copy_except::CopyNamePossessive,
) -> Option<(&'a str, &'a str)> {
    for pos in original
        .char_indices()
        .map(|(pos, _)| pos)
        .chain(std::iter::once(original.len()))
    {
        let boundary = &lower[pos..];
        let Ok((_, boundary_kind)) =
            super::become_copy_except::parse_copy_name_continuation_boundary(boundary, possessive)
        else {
            continue;
        };
        let name_text = original[..pos].trim().trim_matches('"');
        if name_text.is_empty() {
            return None;
        }
        let continuation = match boundary_kind {
            super::become_copy_except::CopyNameBoundary::ContinuationAfterConnector(len) => {
                &original[pos + len..]
            }
            super::become_copy_except::CopyNameBoundary::PunctuationOrEof => &original[pos..],
        };
        return Some((name_text, continuation));
    }
    None
}

fn reconstruct_copy_exception_name(name_text: &str, card_name: Option<&str>) -> Option<String> {
    let lower = name_text.to_lowercase();
    if nom_on_lower(name_text, &lower, |i| {
        all_consuming(value((), tag::<_, _, OracleError<'_>>("~"))).parse(i)
    })
    .is_some()
    {
        return None;
    }
    if let Some(((), rest)) = nom_on_lower(name_text, &lower, |i| {
        value(
            (),
            alt((tag::<_, _, OracleError<'_>>("~'s "), tag("~\u{2019}s "))),
        )
        .parse(i)
    }) {
        if let Some(short_name) = card_name.and_then(comma_short_self_name) {
            let suffix = rest.trim();
            if !suffix.is_empty() {
                return Some(format!("{short_name}'s {suffix}"));
            }
        }
        return None;
    }
    Some(name_text.to_string())
}

/// CR 707.9b + CR 707.2: peel a literal `"named <X>"` rename off a token-copy
/// `, except <body>` clause, returning the original-case name and the body with
/// the `"named <X>"` span removed. Mishra, Eminent One: "…except it's a 4/4
/// Construct artifact creature named Mishra's Warform in addition to its other
/// types." — the name must not be ingested as creature subtypes, and must
/// override the copied name so the legend rule (CR 704.5j) sees the distinct
/// token name.
///
/// Quoted-ability exceptions ("…except it has \"…\"") are left untouched: any
/// `named` inside a granted ability is part of that ability's own text, not a
/// rename of the copy, so the strip is skipped when the body carries a `"`.
fn strip_copy_except_named_override(body: &str) -> (Option<String>, String) {
    if body.contains('"') {
        return (None, body.to_string());
    }
    let lower = body.to_lowercase();
    let tp = TextPair::new(body, &lower);
    let Some((before, after)) = tp.split_around(" named ") else {
        return (None, body.to_string());
    };
    // The literal name runs to the next copy-exception boundary: the additive
    // type carve-out, a further `and`-joined body, or sentence punctuation.
    let mut end = after.original.len();
    for needle in [" in addition to", " and ", " with ", " that ", ",", "."] {
        if let Some(pos) = after.find(needle) {
            end = end.min(pos);
        }
    }
    let name = after.original[..end].trim().trim_matches('"');
    if name.is_empty() {
        return (None, body.to_string());
    }
    // Reassemble the body without the " named <X>" span so the type list parses
    // cleanly ("…artifact creature in addition to its other types").
    let stripped = format!("{}{}", before.original, &after.original[end..]);
    (Some(name.to_string()), stripped)
}

fn parse_token_except_boundary(input: &str) -> OracleResult<'_, &str> {
    alt((
        take_until::<_, _, OracleError<'_>>(", except "),
        take_until::<_, _, OracleError<'_>>(" except "),
    ))
    .parse(input)
}

pub(crate) fn parse_token_description(text: &str) -> Option<TokenDescription> {
    parse_token_description_with_context(text, &ParseContext::default())
}

/// True iff a `for each … this way` count restricts to a specific card type
/// (Dread Summons' "creature card"), so it should override the unfiltered
/// `TrackedSetSize`. A bare/generic "card" filter (e.g. "card discarded this
/// way") is not restrictive and keeps `TrackedSetSize`.
fn tracked_set_count_is_type_restricted(qty: &QuantityRef) -> bool {
    let QuantityRef::FilteredTrackedSetSize { filter, .. } = qty else {
        return false;
    };
    let TargetFilter::Typed(typed) = filter.as_ref() else {
        return false;
    };
    typed
        .type_filters
        .iter()
        .any(|type_filter| !matches!(type_filter, TypeFilter::Card))
}

fn parse_token_description_with_context(
    text: &str,
    ctx: &ParseContext,
) -> Option<TokenDescription> {
    let text = text.trim().trim_end_matches('.');
    let lower = text.to_lowercase();

    // CR 303.7: Strip "attached to [target]" suffix and capture the attachment target.
    let tp = TextPair::new(text, &lower);
    let (text, attach_to) = if let Some((before, after)) = tp.split_around(" attached to ") {
        let (target, _) = parse_target(after.original);
        (before.original, Some(target))
    } else {
        (text, None)
    };

    // CR 508.4 + CR 506.3a: Strip inline "that's tapped and attacking" /
    // "that is tapped and attacking" / "thats tapped and attacking" /
    // "that are tapped and attacking" / "that are attacking" suffix (singular
    // apostrophe variants Oracle normalizes to, plus the plural forms for
    // "create N tokens ...").
    // This is the single-clause form; the trailing "It enters tapped and
    // attacking" sentence form is patched via
    // `ContinuationAst::EntersTappedAttacking`.
    let lower_trimmed = text.to_lowercase();
    // Single combinator for the whole clause: relative-pronoun variants
    // factored into one `alt`, shared tail appears once.
    // CR 107.3: the clause may also be followed by ", where X is …" (e.g. Anim
    // Pakal, Thousandth Moon) — accept that as a valid terminator in addition
    // to EOF so the attacking flag is captured even when a variable-X binding
    // trails the clause.
    // CR 508.4: trailing defender phrases ("that player or a planeswalker they
    // control", "that opponent", etc. — Adeline, Resplendent Cathar / Myriad
    // class) must not prevent the inline modifier from matching; accept a word
    // boundary after "attacking" the same way `parse_battlefield_entry_qualifiers`
    // does for put-onto-battlefield effects.
    let attacking_clause = |i| -> OracleResult<'_, bool> {
        let (i, _) = alt((
            tag(" that's"),
            tag(" that is"),
            tag(" thats"),
            tag(" that are"),
        ))
        .parse(i)?;
        let (i, tapped) = alt((
            value(true, tag(" tapped and attacking")),
            value(false, tag(" attacking")),
        ))
        .parse(i)?;
        let (i, _) = alt((
            value((), nom::combinator::eof),
            value((), tag(", where ")),
            value((), tag(" ")),
            value((), tag(",")),
            value((), tag(".")),
        ))
        .parse(i)?;
        Ok((i, tapped))
    };
    // Nom parses forward; scan byte positions (only those starting with the
    // leading space the clause requires) for the first place where the clause
    // matches. That byte offset is the body length.
    let entry_clause = (0..lower_trimmed.len()).find_map(|pos| {
        (lower_trimmed.as_bytes().get(pos) == Some(&b' '))
            .then(|| {
                attacking_clause(&lower_trimmed[pos..])
                    .ok()
                    .map(|(_, tapped)| (pos, tapped))
            })
            .flatten()
    });
    // When the attacking clause is detected and text is truncated at `pos`, any
    // trailing ", where X is …" that followed the clause is cut off from the
    // token body.  Extract and save it now (from the pre-truncation text) so
    // the X-binding step below can still resolve a variable count.
    let saved_where_x_expr: Option<String> =
        entry_clause.and_then(|(pos, _)| extract_token_where_x_expression(&text[pos..]));
    let (text, enters_attacking, enters_tapped_attacking) = match entry_clause {
        Some((len, tapped)) => (&text[..len], true, tapped),
        None => (text, false, false),
    };
    let (mut count, leading_name, mut rest) =
        if let Some((count, rest)) = parse_token_count_prefix(text) {
            (count, None, rest)
        } else if let Some((name, rest)) = parse_named_token_preamble(text) {
            (QuantityExpr::Fixed { value: 1 }, Some(name), rest)
        } else {
            return None;
        };
    // CR 603.2 + CR 603.4 + CR 107.4: "create that many tokens" on a colored-pip
    // cast trigger (Namor the Sub-Mariner) back-references the cast spell's
    // colored-symbol count (EventSource), not the generic EventContextAmount —
    // a SpellCast event carries no amount, so EventContextAmount resolves to 0.
    // The qualifier color was staged onto the context from the trigger's
    // "with one or more <color> mana symbols in its mana cost" valid_card phrase.
    // Gated on `pending_mana_symbol_count_color`, so Chatterfang-style "that many"
    // counters (color None) are untouched.
    if matches!(
        &count,
        QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount
        }
    ) {
        if let Some(color) = ctx.pending_mana_symbol_count_color {
            count = QuantityExpr::Ref {
                qty: QuantityRef::ManaSymbolsInManaCost {
                    scope: ObjectScope::EventSource,
                    color: Some(color),
                },
            };
        }
    }
    // CR 508.4: Seed `tapped` from the inline "tapped and attacking" suffix
    // detected earlier so the "tapped " / "untapped " leading-word loop below
    // can still flip it if the token text also carries a leading "tapped".
    let mut tapped = enters_tapped_attacking;

    loop {
        let trimmed = rest.trim_start();
        let trimmed_lower = trimmed.to_lowercase();
        if let Some((_, after)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
            value((), tag("tapped ")).parse(i)
        }) {
            tapped = true;
            rest = after;
            continue;
        }
        if let Some((_, after)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
            value((), tag("untapped ")).parse(i)
        }) {
            rest = after;
            continue;
        }
        break;
    }

    let (supertypes, rest_after_supertypes) = strip_token_supertypes(rest);
    rest = rest_after_supertypes;

    let (mut power, mut toughness, rest) =
        if let Ok((rest, (power, toughness))) = nom_primitives::parse_pt_value.parse(rest) {
            (Some(power), Some(toughness), rest.trim_start())
        } else {
            (None, None, rest)
        };

    let (mut colors, rest) = parse_token_color_prefix(rest);
    let (descriptor, suffix) = split_token_head(rest)?;
    let (name_override, suffix) = parse_token_name_clause(suffix);
    // CR 105.1 + CR 105.2: "that's all colors" (Mechtitan Core, etc.) makes the
    // token each of the five colors. Strip the clause before keyword parsing so
    // the trailing keyword ("... and haste that's all colors") still survives,
    // then set the colors.
    let saved_all_colors_where_x_expr = extract_token_where_x_expression(suffix);
    let (suffix, is_all_colors) = strip_token_all_colors_suffix(suffix);
    if is_all_colors {
        colors = ManaColor::ALL.to_vec();
    }
    // CR 107.1a: Parse and apply standalone trailing rounding suffix.
    if let Some(rounding) = parse_rounding_suffix_only(suffix) {
        rewrite_quantity_expr_rounding(&mut count, rounding);
    }
    let mut keywords = parse_token_keyword_clause(suffix);
    let (mut name, types) = parse_token_identity(descriptor, ctx.card_name.as_deref())?;

    // CR 111.4 + CR 111.1: When the token is a registry-defined named token
    // (descriptor is a bare catalog name such as "Vibranium" / "Mutavault" with
    // no inline core type), fill its catalog body characteristics — power,
    // toughness, colors, keywords — that the effect text didn't already
    // specify. CR 111.10 lets the creating effect modify/add to predefined
    // characteristics, so inline P/T, colors, and keywords from the Oracle text
    // take precedence and are never overwritten. The lookup keys on the bare
    // descriptor, so type-bearing descriptors ("Soldier creature") never match
    // a catalog `display_name` and are left untouched.
    if let Some(body) = crate::game::token_presets::known_token_body_by_name_for_source(
        descriptor,
        ctx.card_name.as_deref(),
    ) {
        if power.is_none() {
            power = body.power.map(PtValue::Fixed);
        }
        if toughness.is_none() {
            toughness = body.toughness.map(PtValue::Fixed);
        }
        if colors.is_empty() {
            colors = body.colors.clone();
        }
        for keyword in &body.keywords {
            if !keywords.contains(keyword) {
                keywords.push(keyword.clone());
            }
        }
    }

    if let Some(name_override) = leading_name.or(name_override) {
        name = name_override;
    }

    // CR 107.3: when the attacking clause was stripped and took the ", where X
    // is …" tail with it, `saved_where_x_expr` carries the expression; fall
    // back to it so the variable count is still resolved.
    if let Some(where_expression) = extract_token_where_x_expression(suffix)
        .or(saved_where_x_expr)
        .or(saved_all_colors_where_x_expr)
    {
        // CR 107.3i + CR 117.1: The Token-effect `where X is …` rebind shares
        // the Join-Forces normalization path with non-Token effects via
        // `super::parse_where_x_quantity_expression`. This makes phrases like
        // "the total amount of mana paid this way" (Alliance of Arms) collapse
        // to `QuantityRef::Variable("X")` so the upstream `PayCost { Mana { X } }`
        // loop's accumulated total flows through. Falls back to the CDA path
        // (and then the raw variable name) for phrases neither layer recognizes.
        let binds_x = matches!(&count, QuantityExpr::Ref { qty: QuantityRef::Variable { ref name } } if name == "X")
            || matches!(&power, Some(PtValue::Variable(alias)) if alias == "X")
            || matches!(&toughness, Some(PtValue::Variable(alias)) if alias == "X");
        if binds_x {
            // CR 107.3c: the clause DEFINES X. If the definition is not
            // representable, this token clause does not lower — fail the parse
            // instead of fabricating a raw-text placeholder.
            //
            // The fabricated forms (`PtValue::Variable("<oracle text>")` and
            // `QuantityRef::Variable { name: "<oracle text>" }`) are well-typed
            // but DEAD: nothing resolves a non-`X` variable name, so the token
            // entered as a 0/0 (or with a count of 0) while the raw text still
            // rendered as a supported dynamic quantity in the coverage report —
            // a fabricated green. Honest failure is the only correct answer.
            let bound =
                super::parse_where_x_quantity_expression(&where_expression).or_else(|| {
                    crate::parser::oracle_quantity::parse_cda_quantity(&where_expression)
                })?;
            if matches!(&count, QuantityExpr::Ref { qty: QuantityRef::Variable { ref name } } if name == "X")
            {
                count = bound.clone();
            }
            if matches!(&power, Some(PtValue::Variable(alias)) if alias == "X") {
                power = Some(PtValue::Quantity(bound.clone()));
            }
            if matches!(&toughness, Some(PtValue::Variable(alias)) if alias == "X") {
                toughness = Some(PtValue::Quantity(bound));
            }
        }
    }
    bind_bare_token_x_pt_to_cost_x(&mut power);
    bind_bare_token_x_pt_to_cost_x(&mut toughness);

    if let Some(count_expression) = extract_token_count_expression(suffix) {
        if matches!(&count, QuantityExpr::Ref { qty: QuantityRef::Variable { ref name } } if name == "count")
        {
            // CR 706.2: "the result" (die roll / coin flip) flows through
            // `EventContextAmount`, consistent with `oracle_quantity.rs:1176`.
            // `parse_event_context_quantity` only fires when `parse_cda_quantity`
            // returns None and itself returns None for unrecognized phrases, so
            // it strictly widens coverage without disturbing existing matches.
            // CR 122.1 + CR 608.2c: bind the deferred "a number of" count to the
            // quantity its "equal to <expr>" clause names. An unrepresentable
            // expression FAILS the token clause — the raw-text placeholder it used
            // to fabricate is dead at runtime (game/quantity.rs resolves a non-`X`
            // variable name to 0), so the card created ZERO tokens while still
            // reading as supported.
            count = crate::parser::oracle_quantity::parse_cda_quantity(&count_expression)
                .or_else(|| {
                    crate::parser::oracle_quantity::parse_event_context_quantity(&count_expression)
                })
                .or_else(|| super::parse_where_x_quantity_expression(&count_expression))?;
        }
    }

    // CR 608.2c: "for each [thing] this way" -- the "this way" anaphor counts from
    // the preceding zone moves in the same effect.
    // Matches "for each card put into a graveyard this way", "for each creature
    // exiled this way", etc.
    {
        let suffix_lower = suffix.to_lowercase();
        if suffix_lower.contains("for each") && suffix_lower.contains("this way") {
            // CR 608.2c + CR 205.2a: route ONLY "card type among cards <verb> this
            // way" to the cause-filtered distinct-card-types count (Occult
            // Epiphany #3307); every other "... this way" token keeps
            // `TrackedSetSize`. The dispatch decision is the nom combinator's
            // Ok/Err — the post-"for each " clause is extracted with nom
            // (`take_until` + `tag`), not string-method splitting.
            let after_for_each = take_until::<_, _, OracleError<'_>>("for each ")
                .parse(suffix_lower.as_str())
                .and_then(|(rest, _)| tag("for each ").parse(rest))
                .map(|(clause, _)| clause.trim_end_matches('.').trim());
            count = after_for_each
                .ok()
                .and_then(|clause| {
                    crate::parser::oracle_nom::quantity::parse_distinct_card_types_among_tracked_set(
                        clause,
                    )
                    .ok()
                    .filter(|(rest, _)| rest.is_empty())
                    .map(|(_, qty)| QuantityExpr::Ref { qty })
                    // CR 608.2c + CR 205.2a: a TYPE-restricted "for each <type> card
                    // <verb> this way" (Dread Summons: "for each creature card put
                    // into a graveyard this way") counts only the matching cards
                    // moved this way — `FilteredTrackedSetSize` — not every card
                    // moved (`TrackedSetSize`, which would create X tokens). Only a
                    // restrictive type overrides; a bare/"card" filter keeps
                    // `TrackedSetSize`.
                    .or_else(|| {
                        crate::parser::oracle_quantity::parse_for_each_clause(clause)
                            .filter(tracked_set_count_is_type_restricted)
                            .map(|qty| QuantityExpr::Ref { qty })
                    })
                })
                .unwrap_or(QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                });
        }
    }

    if power.is_none() || toughness.is_none() {
        if let Some(pt_expression) = extract_token_pt_expression(suffix) {
            // CR 107.3c + CR 208.2: a token whose P/T is defined by an
            // expression ("an X/X token, where X is …") must bind that
            // expression to a typed quantity. If it is not representable the
            // token clause does not lower — a raw-text `PtValue::Variable` is a
            // dead node that silently produces a 0/0 token.
            let parsed = crate::parser::oracle_quantity::parse_cda_quantity(&pt_expression)?;
            power = Some(PtValue::Quantity(parsed.clone()));
            toughness = Some(PtValue::Quantity(parsed));
        }
    }

    let is_creature = types.iter().any(|token_type| token_type == "Creature");
    if is_creature && (power.is_none() || toughness.is_none()) {
        if let Some(TokenPtFollowup::PowerToughness {
            power: followup_power,
            toughness: followup_toughness,
        }) = &ctx.token_pt_followup
        {
            power = Some(followup_power.clone());
            toughness = Some(followup_toughness.clone());
        } else {
            return None;
        }
    }

    // Extract quoted static abilities: `and "This token can't block."` / `"~ can't block."`
    let static_abilities = extract_token_static_abilities(suffix, &name);

    Some(TokenDescription {
        name,
        power,
        toughness,
        types,
        supertypes,
        colors,
        keywords,
        tapped,
        count,
        attach_to,
        static_abilities,
        enters_attacking,
    })
}

fn bind_bare_token_x_pt_to_cost_x(value: &mut Option<PtValue>) {
    // CR 107.3a + CR 107.3i + CR 111.3: a bare X in token P/T shares the
    // spell or ability's chosen X unless an explicit "where X is" clause
    // already rebound it above.
    if matches!(value, Some(PtValue::Variable(alias)) if alias == "X") {
        *value = Some(PtValue::Quantity(QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        }));
    }
}

fn parse_token_count_prefix(text: &str) -> Option<(QuantityExpr, &str)> {
    let trimmed = text.trim_start();
    let lower = trimmed.to_lowercase();

    // "that many " -> EventContextAmount
    if let Some((_, rest)) =
        nom_on_lower(trimmed, &lower, |i| value((), tag("that many ")).parse(i))
    {
        return Some((
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            rest,
        ));
    }
    // "a number of " -> deferred count
    if let Some((_, rest)) =
        nom_on_lower(trimmed, &lower, |i| value((), tag("a number of ")).parse(i))
    {
        return Some((
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "count".to_string(),
                },
            },
            rest,
        ));
    }
    // Delegate to parse_count_expr for all numeric/variable/multiplied
    // quantities: "X", "twice X", "three", "half X rounded up", etc.
    let (count, rest) = parse_count_expr(trimmed)?;
    Some((count, rest))
}

fn parse_named_token_preamble(text: &str) -> Option<(String, &str)> {
    // CR 111.4: A named-token preamble is "<Name>, a/an <characteristics> token".
    // The token name may itself contain a comma ("Primo, the Indivisible";
    // "Tibalt, the Fiend-Blooded"), so the FIRST comma is not necessarily the
    // name/body boundary. The boundary is the comma immediately followed by the
    // article that introduces the token's characteristics (", a "/", an "). Scan
    // every comma and pick the one whose remainder begins with an article, so
    // the full epithet stays in the name. Mirrors the article guard already used
    // for the single-comma case.
    for (idx, _) in text.match_indices(',') {
        let after_comma = text[idx + 1..].trim_start();
        let after_lower = after_comma.to_lowercase();
        let Some((_, rest)) =
            nom_on_lower(after_comma, &after_lower, nom_primitives::parse_article)
        else {
            continue;
        };
        let name = text[..idx].trim().trim_matches('"');
        if name.is_empty() {
            continue;
        }
        return Some((name.to_string(), rest));
    }
    None
}

/// CR 205.4a: Strip leading supertype words from the token description and
/// return the captured supertypes alongside the remaining text. Previously the
/// supertypes were discarded; capturing them lets legendary/snow tokens (Marit
/// Lage etc.) carry their supertype through to `Effect::Token` — load-bearing
/// for the legend rule (CR 704.5j).
fn strip_token_supertypes(mut text: &str) -> (Vec<Supertype>, &str) {
    let mut supertypes = Vec::new();
    loop {
        let trimmed = text.trim_start();
        let trimmed_lower = trimmed.to_lowercase();
        let Some((supertype, stripped)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
            alt((
                value(Supertype::Legendary, tag("legendary ")),
                value(Supertype::Snow, tag("snow ")),
                value(Supertype::Basic, tag("basic ")),
            ))
            .parse(i)
        }) else {
            return (supertypes, trimmed);
        };
        if !supertypes.contains(&supertype) {
            supertypes.push(supertype);
        }
        text = stripped;
    }
}

/// Strip a trailing "that's all colors" color clause from a token suffix.
///
/// CR 105.1 + CR 105.2: a token that is "all colors" is each of the five
/// WUBRG colors. The clause appears as a relative-pronoun suffix on the token
/// description (e.g. Mechtitan Core's "... and haste that's all colors" or a
/// bare "... token that's all colors"), so it is detected by scanning word
/// boundaries for the relative-pronoun variants followed by "all colors".
/// Returns the suffix with the clause removed and a flag indicating whether it
/// was present, so the caller can both set the five colors and keep the
/// preceding keyword list intact.
///
/// Building block for the whole class of "create <token> ... that's all colors"
/// effects, not just Mechtitan Core.
fn strip_token_all_colors_suffix(text: &str) -> (&str, bool) {
    fn all_colors_clause(i: &str) -> OracleResult<'_, ()> {
        let (i, _) =
            alt((tag("that's"), tag("that is"), tag("thats"), tag("that are"))).parse(i)?;
        let (i, _) = value((), tag(" all colors")).parse(i)?;
        let (i, _) = alt((value((), nom::combinator::eof), value((), tag(", where ")))).parse(i)?;
        Ok((i, ()))
    }

    let lower = text.to_lowercase();
    if all_colors_clause(&lower).is_ok() {
        return ("", true);
    }

    for (pos, ch) in text.char_indices() {
        if ch != ' ' {
            continue;
        }
        let candidate = &text[pos + ch.len_utf8()..];
        let candidate_lower = candidate.to_lowercase();
        if all_colors_clause(&candidate_lower).is_ok() {
            return (text[..pos].trim_end(), true);
        }
    }

    (text, false)
}

fn parse_token_color_prefix(mut text: &str) -> (Vec<ManaColor>, &str) {
    let mut colors = Vec::new();

    loop {
        let trimmed = text.trim_start();
        let Some((color, rest)) = strip_color_word(trimmed) else {
            break;
        };
        if let Some(color) = color {
            colors.push(color);
        }
        text = rest;

        let trimmed = text.trim_start();
        let trimmed_lower = trimmed.to_lowercase();
        if let Some((_, rest)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
            alt((value((), tag("and ")), value((), tag(", ")))).parse(i)
        }) {
            text = rest;
            continue;
        }
        break;
    }

    (colors, text.trim_start())
}

/// Strip a lowercase color word from the start of text, returning the parsed
/// color and remainder.
///
/// Delegates to `nom_primitives::parse_color` for the five MTG colors, with a
/// manual "colorless" check (which maps to `None` since it's not a `ManaColor`).
/// Note: only matches lowercase color words (matching the original behavior)
/// since token descriptions preserve Oracle casing.
fn strip_color_word(text: &str) -> Option<(Option<ManaColor>, &str)> {
    // "colorless" is not a ManaColor -- handle before delegating to nom
    let text_lower = text.to_lowercase();
    if let Some((_, rest)) =
        nom_on_lower(text, &text_lower, |i| value((), tag("colorless")).parse(i))
    {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return Some((None, rest.trim_start()));
        }
    }
    // Delegate the five named colors to nom combinator.
    // nom's parse_color expects lowercase, and we match only lowercase here
    // (Oracle text preserves original casing in token descriptions).
    if let Ok((rest, color)) = nom_primitives::parse_color.parse(text) {
        // Word boundary: color word must be followed by whitespace or end
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return Some((Some(color), rest.trim_start()));
        }
    }
    None
}

fn split_token_head(text: &str) -> Option<(&str, &str)> {
    let lower = text.to_lowercase();
    let pos = lower.find(" token")?;
    let head = text[..pos].trim();
    let mut suffix = &text[pos + " token".len()..];
    // Strip plural 's' suffix
    if suffix.starts_with('s') {
        suffix = &suffix[1..];
    }
    if head.is_empty() {
        return None;
    }
    Some((head, suffix.trim()))
}

fn parse_token_name_clause(text: &str) -> (Option<String>, &str) {
    let trimmed = text.trim_start();
    let trimmed_lower = trimmed.to_lowercase();
    let Some((_, after_named)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
        value((), tag("named ")).parse(i)
    }) else {
        return (None, trimmed);
    };

    let after_named_lower = after_named.to_lowercase();
    let after_named_tp = TextPair::new(after_named, &after_named_lower);
    let mut end = after_named.len();
    for needle in [" with ", " attached ", ",", "."] {
        if let Some(pos) = after_named_tp.find(needle) {
            end = end.min(pos);
        }
    }

    let name = after_named[..end].trim().trim_matches('"');
    let rest = after_named[end..].trim_start();
    if name.is_empty() {
        (None, rest)
    } else {
        (Some(name.to_string()), rest)
    }
}

/// Extract quoted static abilities from token suffix text.
///
/// Handles patterns like:
/// - `and "This token can't block."` → `[StaticDefinition::new(StaticMode::CantBlock)]`
/// - `and "This creature can't block."` → same
/// - `with 'This token gets +1/+1 for each artifact you control.'` → continuous
///   `BoostByCount`-style modifications.
///
/// Double-quoted spans are unambiguous and parsed greedily. Single-quoted spans
/// only appear when the token-creation effect is itself nested inside a
/// double-quoted activated ability ("This Saga gains \"…create a token with
/// 'X.'\""). They are extracted only via a structurally-anchored single pass:
/// the opening `'` must follow a phrase boundary (`with `, `and `, `or `, or
/// `, `) and the closing `'` is the last `'` in the text. This pairing rule
/// guarantees that any `'` inside the span (apostrophes from "can't" /
/// possessives) is never mistaken for the close quote.
fn extract_token_static_abilities(text: &str, token_name: &str) -> Vec<StaticDefinition> {
    let mut statics = Vec::new();

    // Pass 1: double-quoted abilities — unambiguous delimiters.
    let mut pos = 0;
    while pos < text.len() {
        let Some(start) = text[pos..].find('"') else {
            break;
        };
        let abs_start = pos + start + '"'.len_utf8();
        let Some(end) = text[abs_start..].find('"') else {
            break;
        };
        let quoted = &text[abs_start..abs_start + end];
        push_parsed_statics(quoted.trim(), token_name, &mut statics);
        pos = abs_start + end + '"'.len_utf8();
    }

    // Pass 2: single-quoted abilities (nested inside a double-quoted
    // activated ability). Skipped when double-quoted spans were found —
    // Oracle text never mixes both delimiters at the same nesting level.
    if statics.is_empty() {
        if let Some(span) = find_anchored_single_quoted_span(text) {
            push_parsed_statics(span.trim(), token_name, &mut statics);
        }
    }

    statics
}

fn push_parsed_statics(ability_text: &str, token_name: &str, out: &mut Vec<StaticDefinition>) {
    let normalized;
    let static_text = if token_name.is_empty() {
        ability_text
    } else {
        normalized = normalize_card_name_refs(ability_text, token_name);
        &normalized
    };
    let static_definitions = parse_static_line_multi(static_text);
    if !static_definitions.is_empty() {
        out.extend(static_definitions);
        return;
    }

    let quoted = format!("\"{static_text}\"");
    let modifications = parse_quoted_ability_modifications(&quoted);
    if !modifications.is_empty() {
        out.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(modifications),
        );
    }
}

/// Locate a single-quoted ability span in `text`, returning the content
/// between the open and close quotes (exclusive).
///
/// Anchoring rules (both must hold):
///   - The opening `'` must immediately follow one of the phrase boundaries
///     `with `, `and `, `or `, `, ` — at the start of `text` or preceded by
///     whitespace (so apostrophes embedded in possessives like "creature's"
///     cannot pose as opening quotes).
///   - The closing `'` is the last `'` in `text` (so any internal apostrophe
///     from contractions or possessives is treated as content, not delimiter).
fn find_anchored_single_quoted_span(text: &str) -> Option<&str> {
    let close = text.rfind('\'')?;
    let prefix = &text[..close];

    // Phrase anchors paired (start-of-text form, mid-text form). The mid-text
    // form requires a leading space; the start form does not.
    const ANCHORS: &[(&str, &str)] = &[
        ("with '", " with '"),
        ("and '", " and '"),
        ("or '", " or '"),
        (", '", ", '"),
    ];
    let mut earliest: Option<usize> = None;
    for &(start_anchor, mid_anchor) in ANCHORS {
        if prefix.starts_with(start_anchor) {
            let open = start_anchor.len();
            earliest = Some(earliest.map_or(open, |prev| prev.min(open)));
        }
        if let Some(pos) = prefix.find(mid_anchor) {
            let open = pos + mid_anchor.len();
            earliest = Some(earliest.map_or(open, |prev| prev.min(open)));
        }
    }

    let open = earliest?;
    if close <= open {
        return None;
    }
    Some(&text[open..close])
}

fn extract_token_where_x_expression(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    // The X-expression is a single sentence terminated by the next period.
    // `trim_end_matches('.')` only strips the tail period, which lets trailing
    // sentences ("It gains haste until end of turn.") leak into the extracted
    // expression and poison downstream quantity parsing. Terminate at the
    // first period via `take_until(".")`, falling back to `rest` when the
    // expression has no trailing period.
    let after = tp.strip_after("where x is ")?.original.trim();
    let (_, x_expr) = alt((
        take_until::<_, _, OracleError<'_>>("."),
        rest::<_, OracleError<'_>>,
    ))
    .parse(after)
    .ok()?;
    Some(x_expr.trim().to_string())
}

/// CR 109.4: In a token effect's `for each` clause, a "their <zone>"
/// possessive binds to the player creating the token. The parsed ObjectCount
/// filter comes back with `controller: None` (parse_zone_qual maps "their " to
/// a scope-less `OtherPoss`); stamp `ScopedPlayer` so a per-player "each player
/// creates …" iteration counts each player's OWN zone, not all zones combined.
/// When only the controller creates the token, `ScopedPlayer` falls back to
/// the ability controller at runtime — rules-correct in both cases.
///
/// Called from `try_parse_for_each_effect`'s Token arm in `mod.rs`, which is
/// the single site that lowers "create … token … for each <clause>" to an
/// `Effect::Token` with a dynamic `count`.
pub(super) fn scope_token_for_each_to_iterating_player(expr: QuantityExpr) -> QuantityExpr {
    fn fix_filter(filter: TargetFilter) -> TargetFilter {
        match filter {
            TargetFilter::Typed(tf)
                if tf.controller.is_none()
                    && tf.properties.iter().any(
                        |p| matches!(p, FilterProp::InZone { zone } if *zone != Zone::Battlefield),
                    ) =>
            {
                // `TypedFilter::controller` is `pub`; call it directly. The
                // `None`-guard must live here, so do NOT route through the
                // module-private `inject_controller` (it stamps
                // unconditionally). A filter that already carries a
                // controller, or whose zone is the battlefield, is untouched.
                TargetFilter::Typed(tf.controller(ControllerRef::ScopedPlayer))
            }
            other => other,
        }
    }
    match expr {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } => QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: fix_filter(filter),
            },
        },
        QuantityExpr::Sum { exprs } => QuantityExpr::Sum {
            exprs: exprs
                .into_iter()
                .map(scope_token_for_each_to_iterating_player)
                .collect(),
        },
        QuantityExpr::Max { exprs } => QuantityExpr::Max {
            exprs: exprs
                .into_iter()
                .map(scope_token_for_each_to_iterating_player)
                .collect(),
        },
        other => other,
    }
}

fn extract_token_count_expression(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    Some(
        tp.strip_after("equal to ")?
            .original
            .trim()
            .trim_end_matches('.')
            .to_string(),
    )
}

fn extract_token_pt_expression(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    // SCAN (not anchor) to the "power and toughness" P/T marker anywhere in the
    // token suffix, then accept an optional "are "/"is " copula and the shared
    // "each equal to " tail. `take_until` discards any leading "base " for free,
    // so the combinator subsumes the two prior literals ("… are/is each equal
    // to") AND Skullspore's copula-less "base power and toughness each equal to"
    // — without an anchored `opt(tag("base "))` that would only match at position
    // 0 and silently regress every existing mid-suffix P/T token to 0/0.
    let (_, after) = nom_on_lower(text, &lower, |i| {
        let (i, _) = take_until::<_, _, OracleError<'_>>("power and toughness").parse(i)?;
        let (i, _) = tag("power and toughness ").parse(i)?;
        let (i, _) = opt(alt((tag("are "), tag("is ")))).parse(i)?;
        let (i, _) = tag("each equal to ").parse(i)?;
        Ok((i, ()))
    })?;
    Some(
        after
            .trim()
            .trim_matches('"')
            .trim_end_matches('.')
            .to_string(),
    )
}

fn parse_token_identity(
    descriptor: &str,
    source_name: Option<&str>,
) -> Option<(String, Vec<String>)> {
    let mut core_types = Vec::new();
    let mut subtypes = Vec::new();

    for word in descriptor.split_whitespace() {
        match word.to_lowercase().as_str() {
            "artifact" => push_unique_string(&mut core_types, "Artifact"),
            "creature" => push_unique_string(&mut core_types, "Creature"),
            "enchantment" => push_unique_string(&mut core_types, "Enchantment"),
            "land" => push_unique_string(&mut core_types, "Land"),
            "snow" | "legendary" | "basic" => {}
            _ => subtypes.push(title_case_word(word)),
        }
    }

    if core_types.is_empty() {
        return known_named_token_identity(descriptor, source_name);
    }

    let name = if subtypes.is_empty() {
        "Token".to_string()
    } else {
        subtypes.join(" ")
    };

    let mut types = core_types;
    for subtype in subtypes {
        push_unique_string(&mut types, subtype);
    }

    Some((name, types))
}

fn known_named_token_identity(
    descriptor: &str,
    source_name: Option<&str>,
) -> Option<(String, Vec<String>)> {
    let lower = descriptor.trim().to_lowercase();

    // CR 303.7: Role tokens are Enchantment -- Aura Role tokens.
    if let Some(identity) = known_role_token_identity(&lower) {
        return Some(identity);
    }

    let name = match lower.as_str() {
        "treasure" => "Treasure",
        "food" => "Food",
        "clue" => "Clue",
        "blood" => "Blood",
        "map" => "Map",
        "powerstone" => "Powerstone",
        "junk" => "Junk",
        "shard" => "Shard",
        "gold" => "Gold",
        "lander" => "Lander",
        "mutagen" => "Mutagen",
        // CR 111.4: Any other named token (Vibranium, Mutavault, …) whose
        // identity is catalogued in the predefined-token registry resolves to
        // that catalog body. This generalizes the hardcoded predefined-subtype
        // list above to the entire registry-defined named-token class instead
        // of an allowlist that drops every uncatalogued name to Unimplemented.
        // The simple-artifact predefined subtypes above are kept inline so
        // their canonical name/type-line is independent of catalog presence.
        _ => return known_registry_token_identity(descriptor, source_name),
    };

    Some((
        name.to_string(),
        vec!["Artifact".to_string(), name.to_string()],
    ))
}

/// CR 111.4 + CR 111.1: Resolve a named token's `(display name, type strings)`
/// from the predefined-token registry (`known-tokens.toml`). The type string
/// list follows the parser convention used by [`parse_token_identity`]: core
/// types first (in catalog order), then subtypes. Returns `None` for names not
/// present in the catalog, leaving the token unparsed (Unimplemented) as before.
fn known_registry_token_identity(
    descriptor: &str,
    source_name: Option<&str>,
) -> Option<(String, Vec<String>)> {
    let body =
        crate::game::token_presets::known_token_body_by_name_for_source(descriptor, source_name)?;
    let mut types: Vec<String> = body
        .core_types
        .iter()
        .map(|core| core.to_string())
        .collect();
    for subtype in &body.subtypes {
        push_unique_string(&mut types, subtype);
    }
    Some((body.display_name.clone(), types))
}

/// CR 303.7: Role tokens are predefined Enchantment -- Aura Role tokens with
/// "enchant creature you control". Each Role type grants fixed abilities to the
/// enchanted creature.
fn known_role_token_identity(descriptor: &str) -> Option<(String, Vec<String>)> {
    let name = match descriptor {
        "cursed role" => "Cursed Role",
        "monster role" => "Monster Role",
        "royal role" => "Royal Role",
        "sorcerer role" => "Sorcerer Role",
        "wicked role" => "Wicked Role",
        "young hero role" => "Young Hero Role",
        "virtuous role" => "Virtuous Role",
        "huntsman role" => "Huntsman Role",
        "chef role" => "Chef Role",
        "questing role" => "Questing Role",
        _ => return None,
    };

    Some((
        name.to_string(),
        vec![
            "Enchantment".to_string(),
            "Aura".to_string(),
            "Role".to_string(),
        ],
    ))
}

/// Strip trailing dynamic/attachment clauses from a token "with …" keyword phrase.
fn strip_token_keyword_clause_suffixes(text: &str) -> &str {
    let mut clause = text;
    if let Ok((_, head)) = take_until::<_, _, nom::error::Error<&str>>("\"").parse(clause) {
        clause = head;
    }
    for marker in [" where ", " equal to ", " attached ", " named "] {
        clause = truncate_token_keyword_clause_before(clause, marker);
    }
    clause
}

/// Strip a token keyword clause at the first `marker` (e.g. `" equal to "`).
fn truncate_token_keyword_clause_before<'a>(text: &'a str, marker: &str) -> &'a str {
    let lower = text.to_ascii_lowercase();
    let head_len = match take_until::<_, _, nom::error::Error<&str>>(marker).parse(&lower) {
        Ok((rest, _)) => lower.len() - rest.len(),
        Err(_) => return text,
    };
    &text[..head_len]
}

pub(super) fn parse_token_keyword_clause(text: &str) -> Vec<Keyword> {
    let trimmed = text.trim_start();
    let trimmed_lower = trimmed.to_lowercase();
    let Some((_, after_with)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
        value((), tag("with ")).parse(i)
    }) else {
        return Vec::new();
    };

    let raw_clause = strip_token_keyword_clause_suffixes(after_with)
        .trim()
        .trim_end_matches('.')
        .trim_end_matches(',')
        .trim_end_matches(" and")
        .trim();

    split_token_keyword_list(raw_clause)
        .into_iter()
        .filter_map(map_token_keyword)
        .collect()
}

pub(super) fn split_token_keyword_list(text: &str) -> Vec<&str> {
    text.split(", and ")
        .flat_map(|chunk| chunk.split(" and "))
        .flat_map(|sub| sub.split(", "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

pub(super) fn map_token_keyword(text: &str) -> Option<Keyword> {
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case("all creature types") {
        return Some(Keyword::Changeling);
    }
    match Keyword::from_str(trimmed) {
        Ok(Keyword::Unknown(_)) => {
            super::super::oracle_keyword::parse_keyword_from_oracle(&trimmed.to_lowercase())
        }
        Ok(keyword) => Some(keyword),
        Err(_) => super::super::oracle_keyword::parse_keyword_from_oracle(&trimmed.to_lowercase()),
    }
}

pub(super) fn title_case_word(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub(super) fn push_unique_string(values: &mut Vec<String>, value: impl Into<String> + AsRef<str>) {
    if !values.iter().any(|existing| existing == value.as_ref()) {
        values.push(value.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ObjectScope, QuantityExpr, QuantityRef, RoundingMode, TypeFilter};
    use crate::types::card_type::CoreType;

    #[test]
    fn extract_token_pt_expression_covers_base_and_are_is_copula_classes() {
        // Gap A regression guard (scan-not-anchor). `extract_token_pt_expression`
        // receives the FULL token suffix, so the "power and toughness … each equal
        // to" marker is MID-suffix. The combinator must SCAN to it, not anchor at
        // position 0. Each input is a full suffix; each asserts the trailing
        // expression string (non-vacuous — a bare `is_some` would pass while the
        // anchored mis-implementation regressed the existing tokens to 0/0).
        let cases = [
            // NEW: "base " prefix, no copula (The Skullspore Nexus). Reverting the
            // scan to the original two literals makes this return None.
            (
                "green Fungus Dinosaur creature token with base power and toughness each equal to the total power of those creatures",
                "the total power of those creatures",
            ),
            // EXISTING "are" copula, mid-suffix. Reverting the scan to an anchored
            // `tag("power and toughness ")` at pos 0 makes this return None.
            (
                "0/0 green Ooze creature token with power and toughness are each equal to the number of creatures you control",
                "the number of creatures you control",
            ),
            // EXISTING "is" copula, mid-suffix.
            (
                "green Plant creature token with power and toughness is each equal to your life total",
                "your life total",
            ),
        ];
        for (suffix, expected) in cases {
            assert_eq!(
                extract_token_pt_expression(suffix).as_deref(),
                Some(expected),
                "full-suffix P/T marker must be scanned, not anchored: {suffix:?}"
            );
        }
    }

    #[test]
    fn skullspore_token_lowers_to_triggering_batch_dynamic_pt() {
        // Gap A + Gap B composed. The Skullspore Nexus create clause (verbatim)
        // must lower to a dynamic-P/T token whose base P/T reads the triggering
        // batch's total power. Baseline: `Effect::Unimplemented` (measured).
        use crate::types::ability::{AggregateFunction, ObjectProperty, TrackedAnaphorSource};
        let txt = "Create a green Fungus Dinosaur creature token with base power and toughness each equal to the total power of those creatures.";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("Skullspore token must parse (was Unimplemented)");
        let Effect::Token {
            power,
            toughness,
            types,
            colors,
            count,
            ..
        } = effect
        else {
            panic!("expected Effect::Token, got {effect:?}");
        };
        let expected_pt = PtValue::Quantity(QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetAggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
                source: TrackedAnaphorSource::TriggeringBatch,
            },
        });
        assert_eq!(power, expected_pt.clone(), "base power must be batch sum");
        assert_eq!(toughness, expected_pt, "base toughness must be batch sum");
        assert!(types.iter().any(|t| t == "Creature"));
        assert!(
            types.iter().any(|t| t == "Fungus") && types.iter().any(|t| t == "Dinosaur"),
            "subtypes must include Fungus and Dinosaur, got {types:?}"
        );
        assert_eq!(colors, vec![ManaColor::Green]);
        assert_eq!(count, QuantityExpr::Fixed { value: 1 });
    }

    #[test]
    fn bare_x_x_token_pt_lowers_to_cost_x_quantity_shape() {
        let txt = "Create an X/X green Ooze creature token.";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("expected Token effect");
        let Effect::Token {
            power,
            toughness,
            count,
            types,
            colors,
            ..
        } = effect
        else {
            panic!("expected Effect::Token, got {effect:?}");
        };
        let expected_pt = PtValue::Quantity(QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        });
        assert_eq!(
            power,
            expected_pt.clone(),
            "bare X power must bind to cost X"
        );
        assert_eq!(
            toughness, expected_pt,
            "bare X toughness must bind to cost X"
        );
        assert_eq!(count, QuantityExpr::Fixed { value: 1 });
        assert_eq!(colors, vec![ManaColor::Green]);
        assert!(
            types.iter().any(|t| t == "Creature") && types.iter().any(|t| t == "Ooze"),
            "types must include Creature and Ooze, got {types:?}"
        );
    }

    #[test]
    fn variable_count_and_bare_x_x_token_pt_share_cost_x_shape() {
        let txt = "Create X X/X green Ooze creature tokens.";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("expected Token effect");
        let Effect::Token {
            power,
            toughness,
            count,
            types,
            ..
        } = effect
        else {
            panic!("expected Effect::Token, got {effect:?}");
        };
        let expected_x = QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        };
        assert_eq!(count, expected_x.clone(), "token count must bind to cost X");
        let expected_pt = PtValue::Quantity(expected_x);
        assert_eq!(
            power,
            expected_pt.clone(),
            "bare X power must bind to cost X"
        );
        assert_eq!(
            toughness, expected_pt,
            "bare X toughness must bind to cost X"
        );
        assert!(
            types.iter().any(|t| t == "Creature") && types.iter().any(|t| t == "Ooze"),
            "types must include Creature and Ooze, got {types:?}"
        );
    }

    #[test]
    fn where_x_token_pt_keeps_explicit_greatest_power_quantity_shape() {
        let txt = "Create an X/X green Ooze creature token, where X is the greatest power among creatures you control.";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("expected Token effect");
        let Effect::Token {
            power, toughness, ..
        } = effect
        else {
            panic!("expected Effect::Token, got {effect:?}");
        };
        let expected = crate::parser::oracle_quantity::parse_cda_quantity(
            "the greatest power among creatures you control",
        )
        .expect("greatest-power quantity must parse");
        let expected_pt = PtValue::Quantity(expected);
        assert_eq!(
            power,
            expected_pt.clone(),
            "where-X power must keep the explicit greatest-power quantity"
        );
        assert_eq!(
            toughness, expected_pt,
            "where-X toughness must keep the explicit greatest-power quantity"
        );
    }

    #[test]
    fn where_x_token_pt_covers_known_ooze_source_expressions() {
        let cases = [
            (
                "Create an X/X green Ooze creature token, where X is that spell's mana value.",
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::EventSource,
                    },
                },
            ),
            (
                "Create an X/X green Ooze creature token, where X is the number of +1/+1 counters removed this way.",
                QuantityExpr::Ref {
                    qty: QuantityRef::PreviousEffectAmount {
                        channel: crate::types::ability::DamageChannel::Total,
                    },
                },
            ),
            (
                "Create an X/X green Ooze creature token, where X is the sacrificed creature's power.",
                QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::CostPaidObject,
                    },
                },
            ),
            (
                "Create an X/X green Ooze creature token, where X is this card's power.",
                QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Source,
                    },
                },
            ),
        ];

        for (txt, expected) in cases {
            let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
                .unwrap_or_else(|| panic!("expected Token effect for {txt:?}"));
            let Effect::Token {
                power, toughness, ..
            } = effect
            else {
                panic!("expected Effect::Token, got {effect:?}");
            };
            let expected_pt = PtValue::Quantity(expected);
            assert_eq!(
                power,
                expected_pt.clone(),
                "where-X power must bind for {txt:?}"
            );
            assert_eq!(
                toughness, expected_pt,
                "where-X toughness must bind for {txt:?}"
            );
        }
    }

    #[test]
    fn where_x_token_pt_covers_cards_exiled_this_way_aggregate() {
        use crate::types::ability::{AggregateFunction, ObjectProperty, TrackedAnaphorSource};

        let txt = "Create an X/X blue Zombie creature token, where X is the total power of the cards exiled this way.";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("expected Stitcher Geralf token effect");
        let Effect::Token {
            name,
            power,
            toughness,
            types,
            colors,
            ..
        } = effect
        else {
            panic!("expected Effect::Token, got {effect:?}");
        };
        let expected_pt = PtValue::Quantity(QuantityExpr::Ref {
            qty: QuantityRef::TrackedSetAggregate {
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
                source: TrackedAnaphorSource::ChainSet,
            },
        });
        assert_eq!(name, "Zombie");
        assert!(
            types.iter().any(|t| t == "Creature") && types.iter().any(|t| t == "Zombie"),
            "types must include Creature and Zombie, got {types:?}"
        );
        assert_eq!(colors, vec![ManaColor::Blue]);
        assert_eq!(power, expected_pt.clone());
        assert_eq!(toughness, expected_pt);
    }

    #[test]
    fn occult_epiphany_token_counts_distinct_types_of_discarded() {
        // Occult Epiphany #3307: the token count must be DISTINCT CARD TYPES
        // among the DISCARDED chain members (cause-filtered), NOT TrackedSetSize.
        let txt = "Create a 1/1 white Spirit creature token with flying for each card type among cards discarded this way.";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("expected Token effect");
        let Effect::Token { count, .. } = effect else {
            panic!("expected Effect::Token, got {effect:?}");
        };
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::DistinctCardTypes {
                    source: crate::types::ability::CardTypeSetSource::TrackedSet {
                        caused_by: Some(crate::types::ability::ThisWayCause::Discarded),
                    },
                },
            },
            "Occult Epiphany must count distinct discarded card types, not TrackedSetSize"
        );
    }

    #[test]
    fn bare_for_each_card_discarded_this_way_keeps_tracked_set_size() {
        // No-regression: a plain "for each card discarded this way" token (member
        // count, NOT distinct types) must still resolve to TrackedSetSize.
        let txt = "Create a 1/1 white Spirit creature token with flying for each card discarded this way.";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("expected Token effect");
        let Effect::Token { count, .. } = effect else {
            panic!("expected Effect::Token, got {effect:?}");
        };
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize,
            },
            "bare 'card discarded this way' must keep TrackedSetSize"
        );
    }

    #[test]
    fn for_each_creature_card_this_way_counts_only_creatures() {
        // #4746 Dread Summons: "For each creature card put into a graveyard this
        // way, you create a … token." The token count must restrict to CREATURE
        // cards moved this way (`FilteredTrackedSetSize`), not every card
        // (`TrackedSetSize`, which would create X tokens for X cards milled).
        let txt = "Create a tapped 2/2 black Zombie creature token for each creature card put into a graveyard this way.";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("expected Token effect");
        let Effect::Token { count, .. } = effect else {
            panic!("expected Effect::Token, got {effect:?}");
        };
        let QuantityExpr::Ref {
            qty: QuantityRef::FilteredTrackedSetSize { filter, .. },
        } = &count
        else {
            panic!("expected FilteredTrackedSetSize (creature-restricted), got {count:?}");
        };
        assert!(
            matches!(
                filter.as_ref(),
                TargetFilter::Typed(typed) if typed.type_filters == vec![TypeFilter::Creature]
            ),
            "count must restrict to creature cards milled, got {filter:?}"
        );
    }

    #[test]
    fn copy_x_tokens_of_target_parses_variable_count() {
        // CR 707.2 + CR 107.3: variable X count in copy-token creation.
        let effect = try_parse_token(
            "create x tokens that are copies of target creature you control",
            "Create X tokens that are copies of target creature you control",
            &mut ParseContext::default(),
        )
        .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf { count, .. } = effect else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string()
                }
            }
        );
    }

    #[test]
    fn copy_x_tokens_binds_where_clause() {
        // CR 107.3: X bound to a trailing "where X is <quantity>" clause.
        let txt = "Create X tokens that are copies of target creature you control, where X is the number of Clues you control.";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf { count, .. } = effect else {
            panic!("expected CopyTokenOf")
        };
        let QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(tf),
                },
        } = count
        else {
            panic!("expected where-clause to bind X to an ObjectCount, got {count:?}");
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(
            tf.type_filters
                .contains(&TypeFilter::Subtype("Clue".to_string())),
            "X must count controlled Clues, got {:?}",
            tf.type_filters
        );
    }

    #[test]
    fn copy_tokens_of_exiled_cost_card_use_cost_paid_object_source() {
        let effect = try_parse_token(
            "create two tokens that are copies of the exiled card",
            "Create two tokens that are copies of the exiled card",
            &mut ParseContext::default(),
        )
        .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf { target, count, .. } = effect else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert_eq!(target, TargetFilter::CostPaidObject);
        assert_eq!(count, QuantityExpr::Fixed { value: 2 });
    }

    #[test]
    fn copy_token_with_literal_named_override_emits_set_name() {
        // CR 707.9b + CR 704.5j (issue #4444): Mishra, Eminent One creates a
        // token copy renamed by a literal "named <X>" exception. The override
        // must reach `additional_modifications` as a `SetName` (so the copy of a
        // legendary permanent does not collide with its source under the legend
        // rule), and the name words must NOT leak into the copied subtype list.
        let txt = "create a token that's a copy of target artifact you control, except it's a 4/4 Construct artifact creature named Mishra's Warform in addition to its other types";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf {
            additional_modifications,
            ..
        } = effect
        else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert!(
            additional_modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::SetName { name } if name == "Mishra's Warform"
            )),
            "literal name override must emit SetName with original casing, got {additional_modifications:?}"
        );
        // The name words must not be misclassified as creature subtypes.
        assert!(
            !additional_modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddSubtype { subtype }
                    if matches!(subtype.as_str(), "Named" | "Mishra's" | "Warform")
            )),
            "name words must not leak into the subtype list, got {additional_modifications:?}"
        );
        // The genuine copy exceptions still flow through.
        assert!(additional_modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetPower { value: 4 })));
        assert!(additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddType {
                core_type: CoreType::Artifact
            }
        )));
        assert!(additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddSubtype { subtype } if subtype == "Construct"
        )));
    }

    #[test]
    fn token_keyword_clause_parses_firebending_amount() {
        assert_eq!(
            parse_token_keyword_clause("with firebending 1"),
            vec![Keyword::Firebending(QuantityExpr::Fixed { value: 1 })]
        );
    }

    #[test]
    fn copy_token_of_that_creature_remaps_to_attached_to_for_aura_card() {
        // CR 303.4 + CR 702.103: Inside an Aura/bestow card (Springheart
        // Nantuko), `host_self_reference` is set to `AttachedTo`. The
        // "that creature" anaphor in "create a token that's a copy of that
        // creature" must remap from `ParentTarget` to `AttachedTo` — "that
        // creature" is the enchanted host.
        let mut ctx = ParseContext {
            host_self_reference: Some(TargetFilter::AttachedTo),
            ..ParseContext::default()
        };
        let effect = try_parse_token(
            "create a token that's a copy of that creature",
            "Create a token that's a copy of that creature",
            &mut ctx,
        )
        .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf { target, .. } = effect else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert_eq!(target, TargetFilter::AttachedTo);
    }

    #[test]
    fn copy_token_of_that_creature_keeps_parent_target_for_non_aura_card() {
        // Twinflame Strike class: a non-Aura card leaves `host_self_reference`
        // `None`, so the "that creature" anaphor keeps its `ParentTarget`
        // chosen-target semantics. The Aura-only remap must not corrupt it.
        let effect = try_parse_token(
            "create a token that's a copy of that creature",
            "Create a token that's a copy of that creature",
            &mut ParseContext::default(),
        )
        .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf { target, .. } = effect else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert_eq!(target, TargetFilter::ParentTarget);
    }

    #[test]
    fn copy_token_exception_without_comma_adds_artifact_type() {
        let effect = try_parse_token(
            "create a token that's a copy of that creature except it's an artifact in addition to its other types",
            "Create a token that's a copy of that creature except it's an artifact in addition to its other types",
            &mut ParseContext::default(),
        )
        .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf {
            target,
            additional_modifications,
            ..
        } = effect
        else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert_eq!(target, TargetFilter::ParentTarget);
        assert_eq!(
            additional_modifications,
            vec![ContinuousModification::AddType {
                core_type: CoreType::Artifact,
            }]
        );
    }

    /// CR 707.9b + CR 205.1b: The Apprentice's Folly — elided-subject "is a
    /// Reflection in addition to its other types" in a comma-anded token-copy
    /// except clause must restore `AddSubtype(Reflection)` to
    /// `CopyTokenOf.additional_modifications`. Uses the TRUNCATED text the saga
    /// sentence-splitter produces (no "and has haste" — that is diverted upstream
    /// into a separate Unimplemented sibling, a separate out-of-scope saga bug).
    /// We assert nothing about `extra_keywords`: on this card path there is no
    /// surviving keyword in the clause.
    #[test]
    fn elided_subtype_token_copy_routes_subtype_to_additional_modifications() {
        let effect = try_parse_token(
            "create a token that's a copy of that permanent, except it isn't legendary, is a reflection in addition to its other types",
            "Create a token that's a copy of that permanent, except it isn't legendary, is a Reflection in addition to its other types",
            &mut ParseContext::default(),
        )
        .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf {
            additional_modifications,
            ..
        } = effect
        else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert!(
            additional_modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddSubtype { subtype } if subtype == "Reflection"
            )),
            "AddSubtype(Reflection) must reach CopyTokenOf.additional_modifications; got {additional_modifications:?}"
        );
    }

    #[test]
    fn copy_token_with_name_is_short_self_override_emits_set_name_and_trailing_mods() {
        let txt = "create a token that's a copy of target noncreature artifact you control, except its name is ~'s Warform and it's a 4/4 Construct artifact creature in addition to its other types";
        let mut ctx = ParseContext {
            card_name: Some("Mishra, Eminent One".to_string()),
            ..ParseContext::default()
        };
        let effect =
            try_parse_token(&txt.to_lowercase(), txt, &mut ctx).expect("expected CopyTokenOf");
        let Effect::CopyTokenOf {
            additional_modifications,
            ..
        } = effect
        else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };

        assert!(
            additional_modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::SetName { name } if name == "Mishra's Warform"
            )),
            "short-self name override must reconstruct Mishra's Warform, got {additional_modifications:?}"
        );
        assert!(additional_modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetPower { value: 4 })));
        assert!(additional_modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetToughness { value: 4 })));
        assert!(additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddType {
                core_type: CoreType::Artifact
            }
        )));
        assert!(additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddType {
                core_type: CoreType::Creature
            }
        )));
        assert!(additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddSubtype { subtype } if subtype == "Construct"
        )));
        assert!(
            !additional_modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddSubtype { subtype }
                    if matches!(subtype.as_str(), "Mishra's" | "Warform")
            )),
            "name words must not leak into the subtype list, got {additional_modifications:?}"
        );
    }

    #[test]
    fn copy_token_exact_self_name_is_declines_but_keeps_trailing_mods() {
        let txt = "create a token that's a copy of target artifact you control, except its name is ~ and it's a 4/4 Construct artifact creature in addition to its other types";
        let mut ctx = ParseContext {
            card_name: Some("Mishra, Eminent One".to_string()),
            ..ParseContext::default()
        };
        let effect =
            try_parse_token(&txt.to_lowercase(), txt, &mut ctx).expect("expected CopyTokenOf");
        let Effect::CopyTokenOf {
            additional_modifications,
            ..
        } = effect
        else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert!(
            !additional_modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::SetName { .. })),
            "exact ~ should not name a token after the source; got {additional_modifications:?}"
        );
        assert!(additional_modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetPower { value: 4 })));
        assert!(additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddSubtype { subtype } if subtype == "Construct"
        )));
    }

    #[test]
    fn copy_token_missing_card_name_declines_short_self_but_keeps_trailing_mods() {
        let txt = "create a token that's a copy of target artifact you control, except its name is ~'s Warform and it's a 4/4 Construct artifact creature in addition to its other types";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf {
            additional_modifications,
            ..
        } = effect
        else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert!(
            !additional_modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::SetName { .. })),
            "missing card_name must not guess a short-self name; got {additional_modifications:?}"
        );
        assert!(additional_modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetPower { value: 4 })));
        assert!(additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddSubtype { subtype } if subtype == "Construct"
        )));
    }

    /// Issue #823 — Jace, Mirror Mage: the copy token exception includes both
    /// "not legendary" and a starting-loyalty override. Both are non-keyword
    /// copy exceptions and must reach `CopyTokenOf.additional_modifications`.
    #[test]
    fn jace_copy_token_routes_starting_loyalty_override() {
        let effect = try_parse_token(
            "create a token that's a copy of ~, except it's not legendary and its starting loyalty is 1",
            "create a token that's a copy of ~, except it's not legendary and its starting loyalty is 1",
            &mut ParseContext::default(),
        )
        .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf {
            target,
            additional_modifications,
            ..
        } = effect
        else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert_eq!(target, TargetFilter::SelfRef);
        assert!(additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::RemoveSupertype {
                supertype: Supertype::Legendary
            }
        )));
        assert!(additional_modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::SetStartingLoyalty { value: 1 })));
    }

    /// Issue #1696 — Myrkul, Lord of Bones: "create a token that's a copy of
    /// that card, except it's an enchantment and loses all other card types."
    /// CR 205.1a + CR 707.9d: the "loses all other card types" suffix is the
    /// set-replacement signal, so the copy carries `SetCardTypes`, replacing
    /// (not adding to) the copied creature's card types. The "that card"
    /// anaphor stays `ParentTarget` here (the exile→tracked-set rewrite happens
    /// during chain stitching, exercised by `parse_effect_chain` elsewhere).
    #[test]
    fn myrkul_copy_token_carries_set_card_types_enchantment() {
        let effect = try_parse_token(
            "create a token that's a copy of that card, except it's an enchantment and loses all other card types",
            "Create a token that's a copy of that card, except it's an enchantment and loses all other card types.",
            &mut ParseContext::default(),
        )
        .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf {
            target,
            additional_modifications,
            ..
        } = effect
        else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert_eq!(target, TargetFilter::ParentTarget);
        assert_eq!(
            additional_modifications,
            vec![ContinuousModification::SetCardTypes {
                core_types: vec![CoreType::Enchantment],
            }]
        );
    }

    /// Issue #1424 — The Scarab God activated: 4/4 black Zombie copy exceptions.
    /// CR 707.9d: with no "in addition to its other types" carve-out, color and
    /// creature subtypes REPLACE the copied values — `SetColor` (not `AddColor`)
    /// and `RemoveAllSubtypes { Creature }` + `AddType { Creature }`.
    #[test]
    fn scarab_god_copy_token_carries_pt_color_and_zombie_modifications() {
        let effect = try_parse_token(
            "create a token that's a copy of it, except it's a 4/4 black zombie",
            "Create a token that's a copy of it, except it's a 4/4 black Zombie.",
            &mut ParseContext::default(),
        )
        .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf {
            additional_modifications,
            ..
        } = effect
        else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert!(additional_modifications.contains(&ContinuousModification::SetPower { value: 4 }));
        assert!(
            additional_modifications.contains(&ContinuousModification::SetToughness { value: 4 })
        );
        assert!(additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::SetColor { colors }
                if colors == &vec![ManaColor::Black]
        )));
        assert!(additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::RemoveAllSubtypes {
                set: crate::types::card_type::SubtypeSet::Creature
            }
        )));
        assert!(additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddType {
                core_type: CoreType::Creature
            }
        )));
        assert!(additional_modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddSubtype { subtype } if subtype == "Zombie"
        )));
    }

    #[test]
    fn copy_token_half_pt_exception_emits_dynamic_modifications() {
        let effect = try_parse_token(
            "create two tokens that are copies of that creature, except their power is half that creature's power and their toughness is half that creature's toughness. round up each time",
            "Create two tokens that are copies of that creature, except their power is half that creature's power and their toughness is half that creature's toughness. Round up each time",
            &mut ParseContext::default(),
        )
        .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf {
            target,
            count,
            additional_modifications,
            ..
        } = effect
        else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert_eq!(target, TargetFilter::ParentTarget);
        assert_eq!(count, QuantityExpr::Fixed { value: 2 });
        assert!(matches!(
            additional_modifications.as_slice(),
            [
                ContinuousModification::SetPowerDynamic {
                    value: QuantityExpr::DivideRounded {
                        inner,
                        divisor: 2,
                        rounding: RoundingMode::Up,
                    },
                },
                ContinuousModification::SetToughnessDynamic {
                    value: QuantityExpr::DivideRounded {
                        divisor: 2,
                        rounding: RoundingMode::Up,
                        ..
                    },
                },
            ] if matches!(
                inner.as_ref(),
                QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: ObjectScope::Source
                    }
                }
            )
        ));
    }

    #[test]
    fn token_count_half_x_rounding_after_token_noun_is_applied() {
        let txt = "Create half X Food tokens, rounded up.";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("expected Food token effect");
        let Effect::Token { name, count, .. } = effect else {
            panic!("expected Token, got {effect:?}");
        };
        assert_eq!(name, "Food");
        match count {
            QuantityExpr::DivideRounded {
                inner,
                divisor,
                rounding,
            } => {
                assert_eq!(divisor, 2);
                assert_eq!(rounding, RoundingMode::Up);
                assert!(matches!(
                    inner.as_ref(),
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable { name }
                    } if name == "X"
                ));
            }
            other => panic!("expected DivideRounded token count, got {other:?}"),
        }
    }

    /// CR 109.4: `try_parse_token` emits the default `owner` of
    /// `TargetFilter::Controller`; a "target [player] creates" subject is
    /// lifted into `owner` later by `inject_subject_target` (issue #403).
    #[test]
    fn copy_token_emits_default_controller_owner() {
        let effect = try_parse_token(
            "create a token that's a copy of it",
            "Create a token that's a copy of it",
            &mut ParseContext::default(),
        )
        .expect("expected CopyTokenOf");
        let Effect::CopyTokenOf { owner, target, .. } = effect else {
            panic!("expected CopyTokenOf, got {effect:?}");
        };
        assert_eq!(owner, TargetFilter::Controller);
        // The copy source is left as the context ref — not overwritten.
        assert_eq!(target, TargetFilter::ParentTarget);
    }

    #[test]
    fn scope_token_for_each_stamps_scoped_player_on_their_graveyard() {
        // SUB-FIX A: a `controller: None` ObjectCount on a non-battlefield
        // zone — the shape `parse_for_each_clause_expr` returns for "creature
        // card in their graveyard" — gets ScopedPlayer stamped (CR 109.4).
        use crate::types::ability::{TypeFilter, TypedFilter};
        let parsed = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }],
                }),
            },
        };
        let scoped = scope_token_for_each_to_iterating_player(parsed);
        let QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(tf),
                },
        } = scoped
        else {
            panic!("expected a Typed ObjectCount filter");
        };
        assert_eq!(tf.controller, Some(ControllerRef::ScopedPlayer));
        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
    }

    #[test]
    fn scope_token_for_each_leaves_controllered_and_battlefield_filters_untouched() {
        use crate::types::ability::{TypeFilter, TypedFilter};
        // Already-controllered filter: untouched.
        let already = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: Some(ControllerRef::You),
                    properties: vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }],
                }),
            },
        };
        assert_eq!(
            scope_token_for_each_to_iterating_player(already.clone()),
            already,
        );
        // Battlefield-zone filter: untouched (battlefield is a shared zone).
        let battlefield = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![FilterProp::InZone {
                        zone: Zone::Battlefield,
                    }],
                }),
            },
        };
        assert_eq!(
            scope_token_for_each_to_iterating_player(battlefield.clone()),
            battlefield,
        );
        // Fixed quantity: passes through untouched.
        let fixed = QuantityExpr::Fixed { value: 3 };
        assert_eq!(
            scope_token_for_each_to_iterating_player(fixed.clone()),
            fixed,
        );
    }

    #[test]
    fn keyword_clause_with_trailing_comma_before_where() {
        // "with flying, where X is..." -- comma must not poison the keyword
        let kws = parse_token_keyword_clause("with flying, where X is that spell's mana value");
        assert_eq!(kws, vec![Keyword::Flying]);
    }

    #[test]
    fn keyword_clause_multiple_with_where() {
        let kws =
            parse_token_keyword_clause("with flying and haste, where X is that spell's mana value");
        assert_eq!(kws, vec![Keyword::Flying, Keyword::Haste]);
    }

    #[test]
    fn keyword_clause_no_where() {
        let kws = parse_token_keyword_clause("with flying");
        assert_eq!(kws, vec![Keyword::Flying]);
    }

    /// Issue #2854 (Broodspinner): "with flying equal to …" must not treat the
    /// count clause as part of the keyword name.
    #[test]
    fn keyword_clause_with_equal_to_count_suffix() {
        let kws = parse_token_keyword_clause(
            "with flying equal to the number of card types among cards in your graveyard",
        );
        assert_eq!(kws, vec![Keyword::Flying]);
    }

    /// "with <keyword> named <X>" (Crow Storm, The Hive, etc.): the trailing
    /// "named …" token-name clause must be truncated before keyword parsing so
    /// the keyword survives. Without the " named " marker this yields [].
    #[test]
    fn keyword_clause_with_named_suffix() {
        let kws = parse_token_keyword_clause("with flying named storm crow");
        assert_eq!(kws, vec![Keyword::Flying]);
    }

    /// Hornet Cannon: "with flying and haste named hornet" must keep BOTH.
    #[test]
    fn keyword_clause_multiple_with_named_suffix() {
        let kws = parse_token_keyword_clause("with flying and haste named hornet");
        assert!(kws.contains(&Keyword::Flying), "got {kws:?}");
        assert!(kws.contains(&Keyword::Haste), "got {kws:?}");
    }

    /// Jungle Patrol / Wall of Kelp: "with defender named wall".
    #[test]
    fn keyword_clause_defender_with_named_suffix() {
        let kws = parse_token_keyword_clause("with defender named wall");
        assert_eq!(kws, vec![Keyword::Defender]);
    }

    /// Then Dreadmaws Ate Everyone: "with trample named dreadmaw".
    #[test]
    fn keyword_clause_trample_with_named_suffix() {
        let kws = parse_token_keyword_clause("with trample named dreadmaw");
        assert_eq!(kws, vec![Keyword::Trample]);
    }

    /// No-regression: the " attached " marker must still truncate.
    #[test]
    fn keyword_clause_with_attached_suffix() {
        let kws = parse_token_keyword_clause("with flying attached to it");
        assert_eq!(kws, vec![Keyword::Flying]);
    }

    #[test]
    fn keyword_clause_keeps_numbered_keyword_before_quoted_static() {
        let kws = parse_token_keyword_clause(r#"with toxic 1 and "This token can't block.""#);
        assert_eq!(kws, vec![Keyword::Toxic(1)]);
    }

    #[test]
    fn broodspinner_insect_tokens_with_flying_equal_to_count() {
        let text = "Create a number of 1/1 black and green Insect creature tokens with flying equal to the number of card types among cards in your graveyard.";
        let effect = try_parse_token(text, text, &mut ParseContext::default())
            .expect("Broodspinner token line must parse");
        match effect {
            crate::types::ability::Effect::Token { keywords, .. } => {
                assert!(
                    keywords.contains(&Keyword::Flying),
                    "flying insect tokens must carry Flying, got {keywords:?}"
                );
            }
            other => panic!("expected Token effect, got {other:?}"),
        }
    }

    #[test]
    fn keyword_clause_keeps_keyword_before_all_colors_clause() {
        // CR 105.1/105.2 + CR 702.10: the "that's all colors" clause is stripped
        // before keyword parsing so the trailing keyword survives.
        let (suffix, is_all_colors) =
            strip_token_all_colors_suffix("with flying and haste that's all colors");
        assert!(is_all_colors, "'that's all colors' must be detected");
        assert_eq!(suffix, "with flying and haste");
        let kws = parse_token_keyword_clause(suffix);
        assert_eq!(kws, vec![Keyword::Flying, Keyword::Haste]);
    }

    #[test]
    fn all_colors_suffix_relative_pronoun_variants() {
        // CR 105.1/105.2: each relative-pronoun variant of the all-colors clause
        // is recognized; non-color "that's" clauses are left untouched.
        for clause in [
            "that's all colors",
            "with flying that's all colors",
            "with flying that is all colors",
            "with flying thats all colors",
            "with flying that are all colors",
        ] {
            let (suffix, is_all_colors) = strip_token_all_colors_suffix(clause);
            assert!(is_all_colors, "must detect all-colors in {clause:?}");
            if clause == "that's all colors" {
                assert_eq!(suffix, "");
            } else {
                assert_eq!(suffix, "with flying");
            }
        }
        let (suffix, is_all_colors) =
            strip_token_all_colors_suffix("with flying that's all colors, where X is that value");
        assert!(is_all_colors);
        assert_eq!(suffix, "with flying");
        let (suffix, is_all_colors) =
            strip_token_all_colors_suffix("with flying that's all colors and haste");
        assert!(!is_all_colors);
        assert_eq!(suffix, "with flying that's all colors and haste");
        let (suffix, is_all_colors) = strip_token_all_colors_suffix("with flying");
        assert!(!is_all_colors);
        assert_eq!(suffix, "with flying");
    }

    #[test]
    fn extract_static_cant_block_from_quoted_ability() {
        use crate::types::ability::TargetFilter;
        use crate::types::statics::StaticMode;

        let statics =
            extract_token_static_abilities(r#"with toxic 1 and "This token can't block.""#, "");
        assert_eq!(statics.len(), 1);
        assert_eq!(statics[0].mode, StaticMode::CantBlock);
        assert_eq!(statics[0].affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn extract_static_must_attack_from_named_token_quoted_ability() {
        use crate::types::ability::{TargetFilter, TypedFilter};
        use crate::types::statics::StaticMode;

        let statics = extract_token_static_abilities(
            r#"with flying, indestructible, and "The Void attacks each combat if able.""#,
            "The Void",
        );
        assert_eq!(statics.len(), 1);
        assert_eq!(statics[0].mode, StaticMode::MustAttack);
        assert_eq!(statics[0].affected, Some(TargetFilter::SelfRef));

        let statics = extract_token_static_abilities(
            r#"with "Creatures you control attack each combat if able.""#,
            "Pirate",
        );
        assert_eq!(statics.len(), 1);
        assert_eq!(statics[0].mode, StaticMode::MustAttack);
        assert_eq!(
            statics[0].affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(crate::types::ability::ControllerRef::You),
            ))
        );
    }

    #[test]
    fn extract_static_single_quoted_ability_with_apostrophe_content() {
        use crate::types::ability::TargetFilter;
        use crate::types::statics::StaticMode;

        // Anchored single-quoted span: open `'` follows `and `, close `'`
        // is the last apostrophe. The internal apostrophe in "can't" is
        // treated as content, not a delimiter.
        let statics = extract_token_static_abilities("and '~ can't block.'", "");
        assert_eq!(statics.len(), 1);
        assert_eq!(statics[0].mode, StaticMode::CantBlock);
        assert_eq!(statics[0].affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn extract_static_single_quoted_boost_by_count() {
        // Urza's Saga's chapter II ability: the create-token clause is itself
        // nested inside a double-quoted activated ability, so the granted
        // static uses single quotes. The Construct token must enter with the
        // +1/+1 modifier or it dies to SBAs as a 0/0 immediately.
        let statics = extract_token_static_abilities(
            "with 'This token gets +1/+1 for each artifact you control.'",
            "Construct",
        );
        assert_eq!(
            statics.len(),
            1,
            "expected one continuous static from single-quoted ability, got {statics:?}",
        );
    }

    #[test]
    fn extract_static_empty_when_no_quoted_ability() {
        let statics = extract_token_static_abilities("with flying and haste", "");
        assert!(statics.is_empty());
    }

    #[test]
    fn token_with_quoted_trigger_and_activated_ability_grants_both() {
        let token = parse_token_description(
            "a tapped colorless artifact token named Meteorite with \"When this token enters, it deals 2 damage to any target\" and \"{T}: Add one mana of any color.\"",
        )
        .expect("expected token description");

        assert_eq!(token.name, "Meteorite");
        let modifications: Vec<_> = token
            .static_abilities
            .iter()
            .flat_map(|static_definition| static_definition.modifications.iter())
            .collect();
        assert!(
            modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::GrantTrigger { .. }
            )),
            "expected quoted ETB ability to become a granted trigger: {modifications:?}",
        );
        assert!(
            modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::GrantAbility { .. }
            )),
            "expected quoted tap ability to become a granted activated ability: {modifications:?}",
        );
    }

    #[test]
    fn plural_that_are_tapped_and_attacking_suffix_strips() {
        // CR 508.4 + CR 506.3a: "create two 1/1 white Cat creature tokens that
        // are tapped and attacking" (Leonin Warleader) should set both
        // `tapped` and `enters_attacking` on each token.
        let effect = try_parse_token(
            &"create two 1/1 white cat creature tokens that are tapped and attacking"
                .to_lowercase(),
            "create two 1/1 white Cat creature tokens that are tapped and attacking",
            &mut ParseContext::default(),
        );
        match effect {
            Some(Effect::Token {
                tapped,
                enters_attacking,
                count,
                ..
            }) => {
                assert!(tapped, "plural 'that are' clause must set tapped=true");
                assert!(
                    enters_attacking,
                    "plural 'that are' clause must set enters_attacking=true"
                );
                assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
            }
            other => panic!("Expected Token effect, got {:?}", other),
        }
    }

    #[test]
    fn plural_that_are_attacking_suffix_strips_without_tapping() {
        // CR 508.4: Parhelion II-style tokens enter attacking without being
        // tapped unless the effect explicitly says tapped.
        let effect = try_parse_token(
            &"create two 4/4 white angel creature tokens with flying and vigilance that are attacking"
                .to_lowercase(),
            "create two 4/4 white Angel creature tokens with flying and vigilance that are attacking",
            &mut ParseContext::default(),
        );
        match effect {
            Some(Effect::Token {
                tapped,
                enters_attacking,
                count,
                keywords,
                ..
            }) => {
                assert!(!tapped, "attacking-only clause must not set tapped=true");
                assert!(
                    enters_attacking,
                    "plural 'that are attacking' clause must set enters_attacking=true"
                );
                assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
                assert_eq!(keywords, vec![Keyword::Flying, Keyword::Vigilance]);
            }
            other => panic!("Expected Token effect, got {:?}", other),
        }
    }

    /// CR 706.2: "create a number of Treasure tokens equal to the result"
    /// (Bucknard's Everfull Purse). "the result" of the die roll flows through
    /// `EventContextAmount`, not a `Variable("count")` fallback. Regression for
    /// the count→0 bug where the count was a stringly-typed Variable.
    #[test]
    fn token_count_equal_to_the_result_is_event_context_amount() {
        let effect = try_parse_token(
            "create a number of treasure tokens equal to the result",
            "Create a number of Treasure tokens equal to the result",
            &mut ParseContext::default(),
        )
        .expect("expected Token effect");
        let Effect::Token { count, .. } = effect else {
            panic!("expected Token effect, got {effect:?}");
        };
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            },
            "die-roll result count must resolve to EventContextAmount, not Variable"
        );
    }

    /// CR 205.4a + CR 704.5j: A "legendary" (or "snow"/"basic") supertype in the
    /// inline token grammar must be captured onto `Effect::Token.supertypes`, not
    /// silently stripped. Covers the whole class of legendary tokens (Marit Lage
    /// from Dark Depths, the Pia Nalaar Construct, etc.) so the legend rule
    /// applies. Building-block-level: exercises the supertype-capture path, not a
    /// single card's full Oracle text.
    #[test]
    fn token_captures_legendary_supertype() {
        use crate::types::card_type::Supertype;

        let effect = try_parse_token(
            "create marit lage, a legendary 20/20 black avatar creature token with flying and indestructible",
            "create Marit Lage, a legendary 20/20 black Avatar creature token with flying and indestructible",
            &mut ParseContext::default(),
        )
        .expect("expected Token effect");
        let Effect::Token {
            name,
            supertypes,
            power,
            toughness,
            keywords,
            ..
        } = effect
        else {
            panic!("expected Token effect, got {effect:?}");
        };
        assert_eq!(name, "Marit Lage");
        assert_eq!(
            supertypes,
            vec![Supertype::Legendary],
            "the 'legendary' supertype must be captured, not discarded"
        );
        assert_eq!(power, PtValue::Fixed(20));
        assert_eq!(toughness, PtValue::Fixed(20));
        assert!(keywords.contains(&Keyword::Flying));
        assert!(keywords.contains(&Keyword::Indestructible));
    }

    #[test]
    fn token_with_cant_block_produces_static() {
        let effect = try_parse_token(
            &"create a 1/1 colorless phyrexian mite artifact creature token with toxic 1 and \"this token can't block.\"".to_lowercase(),
            "create a 1/1 colorless Phyrexian Mite artifact creature token with toxic 1 and \"This token can't block.\"",
            &mut ParseContext::default(),
        );
        if let Some(Effect::Token {
            static_abilities, ..
        }) = effect
        {
            assert_eq!(
                static_abilities.len(),
                1,
                "Expected CantBlock static on token"
            );
            assert_eq!(
                static_abilities[0].mode,
                crate::types::statics::StaticMode::CantBlock
            );
        } else {
            panic!("Expected Token effect, got {:?}", effect);
        }
    }

    /// CR 508.4 + CR 107.3: "tokens that are tapped and attacking, where X is
    /// the number of +1/+1 counters on ~" (Anim Pakal, Thousandth Moon).
    /// The ", where X is …" clause used to defeat the eof-anchored scan and
    /// leave `tapped`/`enters_attacking` both false.
    #[test]
    fn tapped_and_attacking_with_trailing_where_x_clause() {
        use crate::types::ability::ObjectScope;
        use crate::types::counter::CounterType;

        let text = "create x 1/1 colorless gnome artifact creature tokens that are tapped and attacking, where x is the number of +1/+1 counters on ~";
        let effect = try_parse_token(
            text,
            "Create X 1/1 colorless Gnome artifact creature tokens that are tapped and attacking, where X is the number of +1/+1 counters on ~",
            &mut ParseContext::default(),
        )
        .expect("expected Token effect");
        let Effect::Token {
            tapped,
            enters_attacking,
            count,
            ..
        } = effect
        else {
            panic!("expected Token effect, got {effect:?}");
        };
        assert!(tapped, "tokens must enter tapped");
        assert!(enters_attacking, "tokens must enter attacking");
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: Some(CounterType::Plus1Plus1),
                    }
                }
            ),
            "X count must resolve to CountersOn(Source, P1P1), got {count:?}"
        );
    }

    /// CR 111.3 + CR 702.10 (Haste) + CR 105.1/105.2 (all five colors):
    /// Mechtitan Core's token has a "with <keywords> that's all colors" suffix
    /// where the "that's all colors" color clause trails the keyword list. The
    /// final keyword ("haste") and the all-five-colors characteristic must both
    /// survive parsing. Building-block regression for the whole class of
    /// "create <token> with <keywords> that's all colors" effects.
    #[test]
    fn token_with_keywords_then_thats_all_colors_keeps_haste_and_colors() {
        use crate::types::mana::ManaColor;

        let text = "create mechtitan, a legendary 10/10 construct artifact creature token with flying, vigilance, trample, lifelink, and haste that's all colors";
        let effect = try_parse_token(
            text,
            "Create Mechtitan, a legendary 10/10 Construct artifact creature token with flying, vigilance, trample, lifelink, and haste that's all colors",
            &mut ParseContext::default(),
        )
        .expect("expected Token effect");
        let Effect::Token {
            keywords, colors, ..
        } = effect
        else {
            panic!("expected Token effect, got {effect:?}");
        };
        assert!(
            keywords.contains(&Keyword::Haste),
            "the trailing keyword before \"that's all colors\" must survive: {keywords:?}",
        );
        for keyword in [
            Keyword::Flying,
            Keyword::Vigilance,
            Keyword::Trample,
            Keyword::Lifelink,
        ] {
            assert!(
                keywords.contains(&keyword),
                "{keyword:?} must be present: {keywords:?}",
            );
        }
        for color in ManaColor::ALL {
            assert!(
                colors.contains(&color),
                "\"that's all colors\" must set {color:?}: {colors:?}",
            );
        }
        assert_eq!(
            colors.len(),
            5,
            "all-colors must be exactly the five WUBRG colors: {colors:?}",
        );
    }

    /// CR 111.3 + CR 105.1/105.2: the all-colors clause may be the whole token
    /// suffix, without a preceding `with <keyword>` list.
    #[test]
    fn token_thats_all_colors_without_keywords_sets_colors() {
        use crate::types::mana::ManaColor;

        let text = "create a 2/2 elemental creature token that's all colors";
        let effect = try_parse_token(
            text,
            "Create a 2/2 Elemental creature token that's all colors",
            &mut ParseContext::default(),
        )
        .expect("expected Token effect");
        let Effect::Token {
            colors, keywords, ..
        } = effect
        else {
            panic!("expected Token effect, got {effect:?}");
        };
        assert!(keywords.is_empty());
        assert_eq!(colors, ManaColor::ALL.to_vec());
    }

    /// CR 105.1/105.2 + CR 107.3: stripping the all-colors suffix must not drop
    /// the trailing `where X is ...` binding for variable token counts.
    #[test]
    fn token_all_colors_where_clause_keeps_x_binding() {
        use crate::types::mana::ManaColor;

        let text = "Create X 1/1 Stained Glass artifact creature tokens that are all colors, where X is the number of creatures you control";
        let effect = try_parse_token(&text.to_lowercase(), text, &mut ParseContext::default())
            .expect("expected Token effect");
        let Effect::Token { colors, count, .. } = effect else {
            panic!("expected Token effect, got {effect:?}");
        };
        assert_eq!(colors, ManaColor::ALL.to_vec());
        let QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(tf),
                },
        } = count
        else {
            panic!("expected where-clause to bind X to an ObjectCount, got {count:?}");
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(
            tf.type_filters.contains(&TypeFilter::Creature),
            "X must count controlled creatures, got {:?}",
            tf.type_filters
        );
    }

    /// CR 508.4 + CR 506.3a: Adeline, Resplendent Cathar — "for each opponent,
    /// create … token that's tapped and attacking that player or a planeswalker
    /// they control." The trailing defender phrase must not defeat the inline
    /// modifier (issue #3303).
    #[test]
    fn token_thats_tapped_and_attacking_that_player_suffix_sets_flags() {
        let text = "create a 1/1 white human creature token that's tapped and attacking that player or a planeswalker they control";
        let effect = try_parse_token(&text.to_lowercase(), text, &mut ParseContext::default())
            .expect("expected Token effect");
        let Effect::Token {
            tapped,
            enters_attacking,
            ..
        } = effect
        else {
            panic!("expected Token effect");
        };
        assert!(tapped, "Human token must enter tapped");
        assert!(
            enters_attacking,
            "Human token must enter attacking despite trailing defender phrase"
        );
    }

    /// CR 111.4 + CR 111.1: A registry-defined named token (here the Mutavault
    /// land token) parses to a complete `Effect::Token` sourced from the
    /// predefined-token catalog, instead of dropping to `Effect::Unimplemented`.
    /// Verifies the registry building block (`known_token_body_by_name`) covers
    /// the whole class of catalog named tokens, not a hardcoded allowlist.
    #[test]
    fn registry_named_land_token_parses_with_tapped() {
        let text = "Create a tapped Mutavault token.";
        let effect = try_parse_token(&text.to_lowercase(), text, &mut ParseContext::default())
            .expect("registry named token must parse, not Unimplemented");
        let Effect::Token {
            name,
            types,
            power,
            toughness,
            tapped,
            count,
            ..
        } = effect
        else {
            panic!("expected Token effect, got {effect:?}");
        };
        assert_eq!(name, "Mutavault");
        assert_eq!(types, vec!["Land".to_string()]);
        // CR 110.5b + CR 603.6d: the leading "tapped " word still flows through.
        assert!(tapped, "leading 'tapped' must set tapped=true");
        // CR 208.3: a noncreature (Land) token has no power/toughness; the
        // create-token default of 0/0 applies.
        assert_eq!(power, PtValue::Fixed(0));
        assert_eq!(toughness, PtValue::Fixed(0));
        assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
    }

    /// CR 111.4 + CR 111.1 + CR 208.1: A registry-defined named *creature* token
    /// (Ajani's Pridemate) fills its catalog power/toughness, color, and types
    /// from the registry body when the Oracle text omits them. Previously the
    /// missing P/T forced the parse to bail (a creature with no P/T returns
    /// None) and the card dropped to Unimplemented.
    #[test]
    fn registry_named_creature_token_fills_body_from_catalog() {
        use crate::types::mana::ManaColor;

        let text = "Create an Ajani's Pridemate token.";
        let effect = try_parse_token(&text.to_lowercase(), text, &mut ParseContext::default())
            .expect("registry named creature token must parse, not Unimplemented");
        let Effect::Token {
            name,
            types,
            power,
            toughness,
            colors,
            ..
        } = effect
        else {
            panic!("expected Token effect, got {effect:?}");
        };
        assert_eq!(name, "Ajani's Pridemate");
        assert!(
            types.contains(&"Creature".to_string())
                && types.contains(&"Cat".to_string())
                && types.contains(&"Soldier".to_string()),
            "catalog core type + subtypes must flow through, got {types:?}"
        );
        // CR 111.10: catalog characteristics fill in for text the effect omitted.
        assert_eq!(power, PtValue::Fixed(2));
        assert_eq!(toughness, PtValue::Fixed(2));
        assert_eq!(colors, vec![ManaColor::White]);
    }

    #[test]
    fn source_defined_named_creature_token_lookup_does_not_invent_fixed_pt() {
        use crate::types::mana::ManaColor;

        let text = "Create a 7/7 Ooze token.";
        let mut ctx = ParseContext {
            card_name: Some("Slime Molding".to_string()),
            ..ParseContext::default()
        };
        let effect = try_parse_token(&text.to_lowercase(), text, &mut ctx)
            .expect("source-defined named Ooze token must parse, not Unimplemented");
        let Effect::Token {
            name,
            types,
            power,
            toughness,
            colors,
            ..
        } = effect
        else {
            panic!("expected Effect::Token, got {effect:?}");
        };

        assert_eq!(name, "Ooze");
        assert!(types.contains(&"Creature".to_string()));
        assert!(types.contains(&"Ooze".to_string()));
        assert_eq!(colors, vec![ManaColor::Green]);
        assert_eq!(power, PtValue::Fixed(7));
        assert_eq!(toughness, PtValue::Fixed(7));
    }

    #[test]
    fn fixed_source_scoped_named_creature_token_still_fills_omitted_pt() {
        use crate::types::mana::ManaColor;

        let text = "Create an Ooze token.";
        let mut ctx = ParseContext {
            card_name: Some("Rot Like the Scum You Are".to_string()),
            ..ParseContext::default()
        };
        let effect = try_parse_token(&text.to_lowercase(), text, &mut ctx)
            .expect("fixed source-scoped Ooze token must parse, not Unimplemented");
        let Effect::Token {
            name,
            types,
            power,
            toughness,
            colors,
            ..
        } = effect
        else {
            panic!("expected Effect::Token, got {effect:?}");
        };

        assert_eq!(name, "Ooze");
        assert!(types.contains(&"Creature".to_string()));
        assert!(types.contains(&"Ooze".to_string()));
        assert_eq!(colors, vec![ManaColor::Green]);
        assert_eq!(power, PtValue::Fixed(2));
        assert_eq!(toughness, PtValue::Fixed(2));
    }

    /// CR 111.1 + CR 111.4 + CR 208.1: ordinary subtype display names are not
    /// token identities when the registry has multiple distinct bodies for the
    /// same name. The Oracle text must supply the missing body characteristics
    /// ("2/2 green Bear creature", "4/4 white Angel creature with flying", etc.)
    /// instead of inheriting whichever catalog entry appears first.
    #[test]
    fn ambiguous_registry_subtype_name_does_not_guess_body() {
        use crate::types::mana::ManaColor;

        assert!(
            crate::game::token_presets::known_token_body_by_name("Bear").is_none(),
            "Bear has multiple catalog bodies and must not pick the first one"
        );

        let text = "Create a Bear token.";
        assert!(
            try_parse_token(&text.to_lowercase(), text, &mut ParseContext::default()).is_none(),
            "a bare ambiguous subtype name must remain unsupported until Oracle text supplies P/T"
        );

        let mut ctx = ParseContext {
            card_name: Some("The Earth King".to_string()),
            ..ParseContext::default()
        };
        let effect = try_parse_token(&text.to_lowercase(), text, &mut ctx)
            .expect("source-scoped Bear token must resolve through the catalog");
        let Effect::Token {
            name,
            types,
            power,
            toughness,
            colors,
            ..
        } = effect
        else {
            panic!("expected Token effect, got {effect:?}");
        };
        assert_eq!(name, "Bear");
        assert_eq!(types, vec!["Creature".to_string(), "Bear".to_string()]);
        assert_eq!(power, PtValue::Fixed(4));
        assert_eq!(toughness, PtValue::Fixed(4));
        assert_eq!(colors, vec![ManaColor::Green]);
    }

    /// CR 111.10: The hardcoded predefined-subtype tokens (Treasure, Food, …)
    /// must keep resolving to their canonical artifact identity even though the
    /// registry fallthrough was added — no regression for the existing class.
    #[test]
    fn predefined_subtype_tokens_still_resolve() {
        for (descriptor, expected) in [
            (
                "Treasure",
                vec!["Artifact".to_string(), "Treasure".to_string()],
            ),
            ("Food", vec!["Artifact".to_string(), "Food".to_string()]),
            ("Clue", vec!["Artifact".to_string(), "Clue".to_string()]),
        ] {
            let (name, types) = known_named_token_identity(descriptor, None)
                .expect("predefined subtype must resolve");
            assert_eq!(name, descriptor);
            assert_eq!(types, expected);
        }
    }
}

#[cfg(test)]
mod kazar_token_landfall_tests {
    use super::*;
    use crate::types::ability::ContinuousModification;

    /// Ka-Zar of the Savage Land's Zabu token: the granted ability text carries
    /// an italicized "Landfall —" ability-word prefix (CR 207.2c) before the
    /// trigger keyword. The token-ability classifier must strip the ability word
    /// and recognize the inner trigger (CR 603.1 / CR 603.6a) as a `GrantTrigger`
    /// static modification — not the `GrantAbility(Unimplemented[landfall])`
    /// catch-all produced before the fix.
    #[test]
    fn zabu_token_landfall_trigger_parses_as_grant_trigger() {
        let txt = "Create Zabu, a legendary 2/2 green Cat creature token with \"Landfall — Whenever a land you control enters, put a +1/+1 counter on Zabu.\"";
        let effect = try_parse_token(&txt.to_lowercase(), txt, &mut ParseContext::default())
            .expect("Zabu token line must parse");
        let Effect::Token {
            name,
            supertypes,
            static_abilities,
            ..
        } = effect
        else {
            panic!("expected Effect::Token, got {effect:?}");
        };
        assert_eq!(name, "Zabu");
        assert!(
            supertypes.contains(&Supertype::Legendary),
            "Zabu must be legendary, got {supertypes:?}"
        );
        let grant_trigger = static_abilities
            .iter()
            .flat_map(|def| def.modifications.iter())
            .find_map(|m| match m {
                ContinuousModification::GrantTrigger { trigger } => Some(trigger),
                _ => None,
            });
        let trigger = grant_trigger.unwrap_or_else(|| {
            panic!("landfall trigger must classify as GrantTrigger, got {static_abilities:#?}")
        });
        // CR 603.6a: the inner trigger is a zone-change (ETB) trigger.
        assert_eq!(
            trigger.mode,
            crate::types::triggers::TriggerMode::ChangesZone
        );
        // No residual Unimplemented landfall effect anywhere in the parsed token.
        assert!(
            // allow-noncombinator: test assertion scanning debug output, not parsing dispatch.
            !format!("{static_abilities:?}").contains("Unimplemented"),
            "token ability must have no residual Unimplemented effect"
        );
    }

    /// The catalog-token path (`inject_catalog_token_abilities`) re-parses the
    /// preset `rules_text` through `classify_quoted_inner`. The Zabu preset's
    /// rules_text begins with the "Landfall —" ability word; the same strip must
    /// apply so the runtime injection yields a `GrantTrigger`, not a
    /// `GrantAbility(Unimplemented)`.
    #[test]
    fn catalog_landfall_rules_text_classifies_as_grant_trigger() {
        let rules_text =
            "Landfall — Whenever a land you control enters, put a +1/+1 counter on Zabu.";
        let mods = crate::parser::oracle_static::classify_quoted_inner(rules_text);
        assert!(
            mods.iter()
                .any(|m| matches!(m, ContinuousModification::GrantTrigger { .. })),
            "catalog rules_text must classify as GrantTrigger, got {mods:?}"
        );
        assert!(
            // allow-noncombinator: test assertion scanning debug output, not parsing dispatch.
            !format!("{mods:?}").contains("Unimplemented"),
            "catalog classification must have no residual Unimplemented effect"
        );
    }

    /// Full-card parse: Ka-Zar's three lines (look at top, play lands from top,
    /// ETB token with landfall) must produce zero residual `Unimplemented`
    /// effects after the ability-word strip fix.
    #[test]
    fn kazar_full_card_no_residual_unimplemented() {
        let oracle = "You may look at the top card of your library any time.\n\
            You may play lands from the top of your library.\n\
            When Ka-Zar of the Savage Land enters, create Zabu, a legendary 2/2 green Cat creature token with \"Landfall — Whenever a land you control enters, put a +1/+1 counter on Zabu.\"";
        let parsed = crate::parser::oracle::parse_oracle_text(
            oracle,
            "Ka-Zar of the Savage Land",
            &[],
            &["Legendary".to_string(), "Creature".to_string()],
            &["Human".to_string(), "Warrior".to_string()],
        );
        let debug = format!("{parsed:?}");
        assert!(
            // allow-noncombinator: test assertion scanning debug output, not parsing dispatch.
            !debug.contains("Unimplemented"),
            "Ka-Zar must parse to zero residual Unimplemented, got: {debug}"
        );
    }
}

#[test]
fn copy_token_non_saga_token_you_control_issue_3294() {
    use crate::types::ability::{ControllerRef, FilterProp, TypeFilter};

    let effect = try_parse_token(
        "create a token that's a copy of a non-saga token you control",
        "Create a token that's a copy of a non-Saga token you control.",
        &mut ParseContext::default(),
    )
    .expect("expected CopyTokenOf");
    let Effect::CopyTokenOf {
        target,
        source_filter,
        ..
    } = effect
    else {
        panic!("expected CopyTokenOf, got {effect:?}");
    };
    assert!(source_filter.is_none());
    let TargetFilter::Typed(tf) = target else {
        panic!("expected Typed copy source, got {target:?}");
    };
    assert!(
        tf.type_filters
            .contains(&TypeFilter::Non(Box::new(TypeFilter::Subtype(
                "Saga".to_string()
            )))),
        "expected Non(Saga), got {:?}",
        tf.type_filters
    );
    assert!(tf.properties.contains(&FilterProp::Token));
    assert_eq!(tf.controller, Some(ControllerRef::You));
}
