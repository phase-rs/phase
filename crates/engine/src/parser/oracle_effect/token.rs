use std::str::FromStr;

use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::{map, opt, rest, value};
use nom::Parser;

use crate::parser::oracle_ir::context::ParseContext;
use crate::parser::oracle_nom::error::OracleResult;
use crate::types::ability::{
    ContinuousModification, ControllerRef, Effect, FilterProp, PtValue, QuantityExpr, QuantityRef,
    StaticDefinition, TargetFilter,
};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_static::{parse_quoted_ability_modifications, parse_static_line_multi};
use super::super::oracle_target::parse_target;
use super::super::oracle_util::{
    normalize_card_name_refs, parse_count_expr, strip_reminder_text, TextPair,
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

pub(super) fn try_parse_token(_lower: &str, text: &str, ctx: &mut ParseContext) -> Option<Effect> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();

    // "create a token that's a copy of {target}"
    if let Ok((_, (tapped, enters_attacking, count))) = parse_copy_token_entry_modifiers(&lower) {
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
            parse_target(target_text)
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
    let token = parse_token_description(after)?;
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
        supertypes: vec![],
        static_abilities: token.static_abilities,
        enter_with_counters: vec![],
    })
}

pub(super) fn parse_copy_token_entry_modifiers(
    input: &str,
) -> OracleResult<'_, (bool, bool, QuantityExpr)> {
    let (rest, _) = tag("create ").parse(input)?;
    let (rest, count) = opt(alt((
        value(
            QuantityExpr::Fixed { value: 1 },
            alt((tag("a "), tag("one "))),
        ),
        map(nom_primitives::parse_number, |value| QuantityExpr::Fixed {
            value: value as i32,
        }),
    )))
    .parse(rest)?;
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
    let Ok((except_input, head_lower)) = parse_token_except_boundary(&lower) else {
        return (text, Vec::new(), Vec::new());
    };
    let head = &text[..head_lower.len()];
    // Pass the lowercase suffix starting at `[, ]except ` to the shared
    // building block. The except parser is the single authority for the
    // grammar (CR 707.9 + CR 707.2): keyword lists, supertype additions /
    // removals, conditional counter placement, etc.
    let card_name = ""; // SetName cannot apply to token-copy (source unknown at parse time).
    let (_, modifications) =
        match super::become_copy_except::parse_except_clause(except_input, card_name, ctx) {
            Some(parts) => parts,
            None => return (head, Vec::new(), Vec::new()),
        };

    let mut extra_keywords = Vec::new();
    let mut additional_modifications = Vec::new();
    for modification in modifications {
        match modification {
            ContinuousModification::AddKeyword { keyword } => extra_keywords.push(keyword),
            other => additional_modifications.push(other),
        }
    }
    (head, extra_keywords, additional_modifications)
}

fn parse_token_except_boundary(input: &str) -> OracleResult<'_, &str> {
    alt((
        take_until::<_, _, OracleError<'_>>(", except "),
        take_until::<_, _, OracleError<'_>>(" except "),
    ))
    .parse(input)
}

pub(crate) fn parse_token_description(text: &str) -> Option<TokenDescription> {
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
    // factored into one `alt`, shared tail appears once, `eof` anchors the
    // match at the string's end.
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
        let (i, _) = nom::combinator::eof(i)?;
        Ok((i, tapped))
    };
    // Nom parses forward; scan byte positions (only those starting with the
    // leading space the clause requires) for the first place where the clause
    // consumes the remainder to EOF. That byte offset is the body length.
    let entry_clause = (0..lower_trimmed.len()).find_map(|pos| {
        (lower_trimmed.as_bytes().get(pos) == Some(&b' '))
            .then(|| {
                attacking_clause(&lower_trimmed[pos..])
                    .ok()
                    .map(|(_, tapped)| (pos, tapped))
            })
            .flatten()
    });
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

    rest = strip_token_supertypes(rest);

    let (mut power, mut toughness, rest) =
        if let Ok((rest, (power, toughness))) = nom_primitives::parse_pt_value.parse(rest) {
            (Some(power), Some(toughness), rest.trim_start())
        } else {
            (None, None, rest)
        };

    let (colors, rest) = parse_token_color_prefix(rest);
    let (descriptor, suffix) = split_token_head(rest)?;
    let (name_override, suffix) = parse_token_name_clause(suffix);
    let keywords = parse_token_keyword_clause(suffix);
    let (mut name, types) = parse_token_identity(descriptor)?;

    if let Some(name_override) = leading_name.or(name_override) {
        name = name_override;
    }

    if let Some(where_expression) = extract_token_where_x_expression(suffix) {
        // CR 107.3i + CR 117.1: The Token-effect `where X is …` rebind shares
        // the Join-Forces normalization path with non-Token effects via
        // `super::parse_where_x_quantity_expression`. This makes phrases like
        // "the total amount of mana paid this way" (Alliance of Arms) collapse
        // to `QuantityRef::Variable("X")` so the upstream `PayCost { Mana { X } }`
        // loop's accumulated total flows through. Falls back to the CDA path
        // (and then the raw variable name) for phrases neither layer recognizes.
        let resolve_count = || {
            super::parse_where_x_quantity_expression(&where_expression)
                .or_else(|| crate::parser::oracle_quantity::parse_cda_quantity(&where_expression))
        };
        if matches!(&count, QuantityExpr::Ref { qty: QuantityRef::Variable { ref name } } if name == "X")
        {
            count = resolve_count().unwrap_or_else(|| QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: where_expression.clone(),
                },
            });
        }
        if matches!(&power, Some(PtValue::Variable(alias)) if alias == "X") {
            power = Some(
                resolve_count()
                    .map(PtValue::Quantity)
                    .unwrap_or_else(|| PtValue::Variable(where_expression.clone())),
            );
        }
        if matches!(&toughness, Some(PtValue::Variable(alias)) if alias == "X") {
            toughness = Some(
                resolve_count()
                    .map(PtValue::Quantity)
                    .unwrap_or_else(|| PtValue::Variable(where_expression)),
            );
        }
    }

    if let Some(count_expression) = extract_token_count_expression(suffix) {
        if matches!(&count, QuantityExpr::Ref { qty: QuantityRef::Variable { ref name } } if name == "count")
        {
            count = crate::parser::oracle_quantity::parse_cda_quantity(&count_expression)
                .unwrap_or(QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: count_expression,
                    },
                });
        }
    }

    // CR 609.3: "for each [thing] this way" -- count from preceding zone moves.
    // Matches "for each card put into a graveyard this way", "for each creature
    // exiled this way", etc.
    {
        let suffix_lower = suffix.to_lowercase();
        if suffix_lower.contains("for each") && suffix_lower.contains("this way") {
            count = QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize,
            };
        }
    }

    if power.is_none() || toughness.is_none() {
        if let Some(pt_expression) = extract_token_pt_expression(suffix) {
            let parsed = crate::parser::oracle_quantity::parse_cda_quantity(&pt_expression);
            power = Some(
                parsed
                    .clone()
                    .map(PtValue::Quantity)
                    .unwrap_or_else(|| PtValue::Variable(pt_expression.clone())),
            );
            toughness = Some(
                parsed
                    .map(PtValue::Quantity)
                    .unwrap_or_else(|| PtValue::Variable(pt_expression)),
            );
        }
    }

    let is_creature = types.iter().any(|token_type| token_type == "Creature");
    if is_creature && (power.is_none() || toughness.is_none()) {
        return None;
    }

    // Extract quoted static abilities: `and "This token can't block."` / `"~ can't block."`
    let static_abilities = extract_token_static_abilities(suffix, &name);

    Some(TokenDescription {
        name,
        power,
        toughness,
        types,
        colors,
        keywords,
        tapped,
        count,
        attach_to,
        static_abilities,
        enters_attacking,
    })
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
    let comma = text.find(',')?;
    let name = text[..comma].trim().trim_matches('"');
    if name.is_empty() {
        return None;
    }

    let after_comma = text[comma + 1..].trim_start();
    let after_lower = after_comma.to_lowercase();
    let (_, rest) = nom_on_lower(after_comma, &after_lower, nom_primitives::parse_article)?;
    Some((name.to_string(), rest))
}

fn strip_token_supertypes(mut text: &str) -> &str {
    loop {
        let trimmed = text.trim_start();
        let trimmed_lower = trimmed.to_lowercase();
        let Some((_, stripped)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
            alt((
                value((), tag("legendary ")),
                value((), tag("snow ")),
                value((), tag("basic ")),
            ))
            .parse(i)
        }) else {
            return trimmed;
        };
        text = stripped;
    }
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
    let tp = TextPair::new(text, &lower);
    for needle in [
        "power and toughness are each equal to ",
        "power and toughness is each equal to ",
    ] {
        if let Some(after) = tp.strip_after(needle) {
            return Some(
                after
                    .original
                    .trim()
                    .trim_matches('"')
                    .trim_end_matches('.')
                    .to_string(),
            );
        }
    }
    None
}

fn parse_token_identity(descriptor: &str) -> Option<(String, Vec<String>)> {
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
        return known_named_token_identity(descriptor);
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

fn known_named_token_identity(descriptor: &str) -> Option<(String, Vec<String>)> {
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
        _ => return None,
    };

    Some((
        name.to_string(),
        vec!["Artifact".to_string(), name.to_string()],
    ))
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

pub(super) fn parse_token_keyword_clause(text: &str) -> Vec<Keyword> {
    let trimmed = text.trim_start();
    let trimmed_lower = trimmed.to_lowercase();
    let Some((_, after_with)) = nom_on_lower(trimmed, &trimmed_lower, |i| {
        value((), tag("with ")).parse(i)
    }) else {
        return Vec::new();
    };

    let raw_clause = after_with
        .split('"')
        .next()
        .unwrap_or(after_with)
        .split(" where ")
        .next()
        .unwrap_or(after_with)
        .split(" attached ")
        .next()
        .unwrap_or(after_with)
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
    use crate::types::ability::{ObjectScope, QuantityExpr, QuantityRef, RoundingMode};
    use crate::types::card_type::CoreType;

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
}
