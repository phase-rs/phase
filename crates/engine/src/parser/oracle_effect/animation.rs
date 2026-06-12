use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::{tag, tag_no_case, take_till, take_until};
use nom::character::complete::{multispace0, multispace1, satisfy};
use nom::combinator::{eof, opt, peek, recognize, value};
use nom::multi::{many0, separated_list1};
use nom::sequence::{pair, preceded};
use nom::Parser;

use super::super::oracle_nom::error::{oracle_err, OracleError, OracleResult};
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_util::split_around;
use super::token::{
    map_token_keyword, push_unique_string, split_token_keyword_list, title_case_word,
};
use crate::parser::oracle_ir::ast::*;
use crate::parser::oracle_ir::context::ParseContext;
use crate::parser::oracle_quantity;
use crate::types::ability::{PtValue, QuantityExpr, QuantityRef};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;

pub(crate) fn parse_animation_spec(text: &str, _ctx: &mut ParseContext) -> Option<AnimationSpec> {
    let lower = text.to_lowercase();
    if lower.contains(" copy of ")
        || lower.contains(" of your choice")
        || lower.contains(" all activated abilities ")
        || lower.contains(" loses all other card types ")
        || lower.contains(" all colors")
    {
        return None;
    }

    let mut spec = AnimationSpec::default();
    let mut rest = text.trim().trim_end_matches('.');

    // Check for ability-loss suffixes using pre-lowered text
    let rest_lower = rest.to_lowercase();
    for suffix in [
        " and loses all other abilities",
        " and it loses all other abilities",
        " and loses all abilities",
    ] {
        if rest_lower.ends_with(suffix) {
            let end = rest.len() - suffix.len();
            rest = rest[..end].trim_end_matches(',').trim();
            spec.remove_all_abilities = true;
            break;
        }
    }

    if let Some(stripped) = rest.strip_prefix("a ") {
        rest = stripped;
    } else if let Some(stripped) = rest.strip_prefix("an ") {
        rest = stripped;
    }

    // CR 107.3c: "X/X where X is ~'s power" — X is bound to a dynamic quantity
    // (source's power), NOT the cost paid. This pattern must be detected BEFORE
    // the X-cost activation path (parse_cost_x_become_pt_prefix) to avoid the
    // false match that would emit CostXPaid (which evaluates to 0 for triggers).
    // Covers Obuun, Mul Daya Ancestor and similar patterns.
    // allow-noncombinator: case-insensitive phrase scan - nom lacks case-insensitive take_until
    if let Some(where_x_pos) = rest.to_lowercase().find("where x is ") {
        let before_where = rest[..where_x_pos].trim_end_matches(',').trim();
        let after_where = &rest[where_x_pos + "where x is ".len()..];
        let after_where_lower = after_where.to_lowercase();
        rest = parse_cost_x_become_pt_prefix(before_where).unwrap_or(before_where);

        let qty = oracle_quantity::parse_quantity_ref(&after_where_lower)?;
        let dynamic_qty = QuantityExpr::Ref { qty };
        spec.dynamic_power = Some(dynamic_qty.clone());
        spec.dynamic_toughness = Some(dynamic_qty);
    }

    if let Some((power, toughness, after_pt)) = parse_fixed_become_pt_prefix(rest) {
        spec.power = Some(power);
        spec.toughness = Some(toughness);
        rest = after_pt;
    } else if spec.dynamic_power.is_none() && spec.dynamic_toughness.is_none() {
        // Only apply X-cost activation path if dynamic P/T wasn't already set by
        // "where X is" pattern detection above. This prevents CostXPaid from
        // overwriting SourcePower for patterns like "X/X where X is ~'s power".
        if let Some(after_pt) = parse_cost_x_become_pt_prefix(rest) {
            // CR 107.3 + CR 107.3a: "{X}{G}: ~ becomes an X/X creature" — P/T resolves
            // to the X paid for the activation cost. Maps Variable("X")/Variable("X") to
            // CostXPaid so the animate effect reads cost_x_paid at resolution (not Variable
            // which only resolves while the spell/ability is on the stack).
            let cost_x = QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            };
            spec.dynamic_power = Some(cost_x.clone());
            spec.dynamic_toughness = Some(cost_x);
            rest = after_pt;
        }
    }

    if let Some((descriptor, power, toughness, keywords)) = split_animation_base_pt_clause(rest) {
        spec.power = Some(power);
        spec.toughness = Some(toughness);
        spec.keywords.extend(keywords);
        rest = descriptor;
    }

    if spec.dynamic_power.is_none() && spec.dynamic_toughness.is_none() {
        // Only apply split_animation_dynamic_pt_clause if dynamic P/T wasn't already
        // set by "where X is" pattern detection above. This prevents overwriting
        // SourcePower with other quantity references.
        if let Some((descriptor, value)) = split_animation_dynamic_pt_clause(rest) {
            spec.dynamic_power = Some(value.clone());
            spec.dynamic_toughness = Some(value);
            rest = descriptor;
        }
    }

    let (descriptor, keywords) = split_animation_keyword_clause(rest);
    spec.keywords.extend(keywords);
    rest = descriptor;

    if let Some((colors, after_colors)) = parse_animation_color_prefix(rest) {
        spec.colors = Some(colors);
        rest = after_colors;
    }

    spec.types = parse_animation_types(
        rest,
        spec.power.is_some()
            || spec.toughness.is_some()
            || spec.dynamic_power.is_some()
            || spec.dynamic_toughness.is_some(),
    );

    if spec.power.is_none()
        && spec.toughness.is_none()
        && spec.dynamic_power.is_none()
        && spec.dynamic_toughness.is_none()
        && spec.colors.is_none()
        && spec.keywords.is_empty()
        && spec.types.is_empty()
        && !spec.remove_all_abilities
    {
        None
    } else {
        Some(spec)
    }
}

pub(crate) fn animation_modifications(
    spec: &AnimationSpec,
) -> Vec<crate::types::ability::ContinuousModification> {
    use crate::types::ability::ContinuousModification;
    use crate::types::card_type::CoreType;

    let mut modifications = Vec::new();

    if let Some(power) = spec.power {
        modifications.push(ContinuousModification::SetPower { value: power });
    }
    if let Some(toughness) = spec.toughness {
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }
    if let Some(value) = &spec.dynamic_power {
        modifications.push(ContinuousModification::SetPowerDynamic {
            value: value.clone(),
        });
    }
    if let Some(value) = &spec.dynamic_toughness {
        modifications.push(ContinuousModification::SetToughnessDynamic {
            value: value.clone(),
        });
    }
    if let Some(colors) = &spec.colors {
        modifications.push(ContinuousModification::SetColor {
            colors: colors.clone(),
        });
    }
    if spec.remove_all_abilities {
        modifications.push(ContinuousModification::RemoveAllAbilities);
    }
    for keyword in &spec.keywords {
        modifications.push(ContinuousModification::AddKeyword {
            keyword: keyword.clone(),
        });
    }
    for type_name in &spec.types {
        if let Ok(core_type) = CoreType::from_str(type_name) {
            modifications.push(ContinuousModification::AddType { core_type });
        } else {
            modifications.push(ContinuousModification::AddSubtype {
                subtype: type_name.clone(),
            });
        }
    }

    modifications
}

/// CR 205.1a + CR 613.1d (Layer 4): Build the layer-4 modifications for a
/// "[subject] becomes a [type]" animation, applying SUBTYPE **replacement** (not
/// addition) unless the effect is "in addition to its other types".
///
/// `animation_modifications` is the purely-additive base (the CR 205.1b
/// "in addition" reading). When `is_additive` is false, CR 205.1a applies: a
/// granted subtype replaces the existing subtypes from the same set — so a
/// creature that "becomes a Frog" ends up with subtypes exactly `[Frog]` rather
/// than retaining its prior creature types (Human, Soldier, …). This injects a
/// `RemoveAllSubtypes` for each affected subtype set before its first
/// `AddSubtype`.
///
/// Card TYPES stay additive (`AddType`): an animated permanent keeps its other
/// card types (e.g. an animated land remains a land, a creature stays a
/// creature) — only the subtype dimension is replaced. This matches the
/// behavior the combined pump+become path already produces.
pub(crate) fn animation_modifications_with_replacement(
    spec: &AnimationSpec,
    is_additive: bool,
) -> Vec<crate::types::ability::ContinuousModification> {
    use crate::types::ability::ContinuousModification;
    use crate::types::card_type::{noncreature_subtype_set, SubtypeSet};

    let base = animation_modifications(spec);
    if is_additive {
        return base;
    }

    let mut out = Vec::with_capacity(base.len() + 1);
    let mut removed_sets: Vec<SubtypeSet> = Vec::new();
    for modification in base {
        // CR 205.1a: a granted subtype replaces existing subtypes from its set —
        // inject `RemoveAllSubtypes` once per affected set before the AddSubtype.
        if let ContinuousModification::AddSubtype { subtype } = &modification {
            let set = noncreature_subtype_set(subtype).unwrap_or(SubtypeSet::Creature);
            if !removed_sets.contains(&set) {
                out.push(ContinuousModification::RemoveAllSubtypes { set });
                removed_sets.push(set);
            }
        }
        out.push(modification);
    }
    out
}

/// Parse a color word prefix from animation text, handling "colorless" and
/// the five MTG colors.
///
/// Delegates color word recognition to `nom_primitives::parse_color` for the
/// five named colors, with manual handling for "colorless" (no `ManaColor`).
fn parse_animation_color_prefix(text: &str) -> Option<(Vec<ManaColor>, &str)> {
    let mut rest = text.trim_start();
    let mut saw_color = false;
    let mut colors = Vec::new();

    loop {
        if let Some(stripped) = strip_prefix_word(rest, "colorless") {
            saw_color = true;
            rest = stripped;
        } else {
            // Delegate the five named colors to nom combinator
            let lower = rest.to_lowercase();
            if let Ok((rest_lower, color)) = nom_primitives::parse_color.parse(&lower) {
                let consumed = lower.len() - rest_lower.len();
                let after = &rest[consumed..];
                // Word boundary: color word must be followed by whitespace or end
                if after.is_empty() || after.starts_with(char::is_whitespace) {
                    saw_color = true;
                    colors.push(color);
                    rest = after.trim_start();
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        if let Some(stripped) = rest.strip_prefix("and ") {
            rest = stripped;
            continue;
        }
        break;
    }

    saw_color.then_some((colors, rest.trim_start()))
}

fn strip_prefix_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(word)?;
    if rest.is_empty() {
        Some(rest)
    } else if rest.starts_with(' ') {
        Some(rest.trim_start())
    } else {
        None
    }
}

pub(super) fn parse_fixed_become_pt_prefix(text: &str) -> Option<(i32, i32, &str)> {
    let (rest, (power, toughness)) = nom_primitives::parse_pt_value.parse(text).ok()?;
    match (power, toughness) {
        (
            crate::types::ability::PtValue::Fixed(power),
            crate::types::ability::PtValue::Fixed(toughness),
        ) => Some((power, toughness, rest.trim_start())),
        _ => None,
    }
}

/// CR 107.3 + CR 107.3a: Recognize "X/X" at the start of a "becomes" descriptor
/// and map it to the CostXPaid dynamic quantity (the X paid in the activation cost).
///
/// Returns the remainder after the "X/X" token when both power and toughness
/// are `Variable("X")`. Returns `None` for `*/*`, fixed P/T, or asymmetric X
/// (e.g. "X/1" — not yet supported in this path, falls through to other parsers).
///
/// This enables X-cost creature-land animate abilities like "{X}{G}: Until end
/// of turn, ~ becomes an X/X green Hydra creature" (Lair of the Hydra) to
/// produce SetPowerDynamic + SetToughnessDynamic modifications keyed to
/// CostXPaid.
fn parse_cost_x_become_pt_prefix(text: &str) -> Option<&str> {
    let (rest, (power, toughness)) = nom_primitives::parse_pt_value.parse(text).ok()?;
    match (power, toughness) {
        (PtValue::Variable(ref p), PtValue::Variable(ref t))
            if p.eq_ignore_ascii_case("x") && t.eq_ignore_ascii_case("x") =>
        {
            Some(rest.trim_start())
        }
        _ => None,
    }
}

/// CR 613.1d/f/g: animation clauses can simultaneously change types, grant
/// keyword abilities, and set base P/T. Keep those written components together
/// so later lowering emits Layer 4, Layer 6, and Layer 7 modifications.
fn split_animation_base_pt_clause(text: &str) -> Option<(&str, i32, i32, Vec<Keyword>)> {
    let lower = text.to_lowercase();
    let (_, (descriptor_lower, power, toughness, keywords)) =
        parse_animation_base_pt_clause(&lower).ok()?;
    let descriptor = text[..descriptor_lower.len()].trim_end_matches(',').trim();
    Some((descriptor, power, toughness, keywords))
}

fn parse_animation_base_pt_clause(input: &str) -> OracleResult<'_, (&str, i32, i32, Vec<Keyword>)> {
    let (rest, descriptor) = take_until(" with base power and toughness ").parse(input)?;
    let (rest, _) = tag(" with base power and toughness ").parse(rest)?;
    let (rest, (power, toughness)) = nom_primitives::parse_pt_value.parse(rest)?;
    let (power, toughness) = match (power, toughness) {
        (PtValue::Fixed(power), PtValue::Fixed(toughness)) => (power, toughness),
        _ => return Err(oracle_err(rest)),
    };
    let (rest, keywords) = opt(parse_base_pt_trailing_keywords).parse(rest)?;
    Ok((
        rest,
        (descriptor, power, toughness, keywords.unwrap_or_default()),
    ))
}

fn parse_base_pt_trailing_keyword_intro(input: &str) -> OracleResult<'_, ()> {
    let (rest, _) = multispace0(input)?;
    let (rest, _) = alt((tag(", and "), tag(", "), tag("and "))).parse(rest)?;
    let (rest, _) = opt(alt((
        tag("has "),
        tag("have "),
        tag("gains "),
        tag("gain "),
    )))
    .parse(rest)?;
    Ok((rest, ()))
}

fn parse_base_pt_trailing_keywords(input: &str) -> OracleResult<'_, Vec<Keyword>> {
    let (rest, _) = parse_base_pt_trailing_keyword_intro(input)?;
    let (rest, raw_clause) = take_till(|c| c == '"' || c == '.').parse(rest)?;
    let keywords = split_token_keyword_list(raw_clause.trim())
        .into_iter()
        .filter_map(map_token_keyword)
        .collect();
    Ok((rest, keywords))
}

fn parse_dynamic_pt_clause(input: &str) -> OracleResult<'_, (&str, QuantityExpr)> {
    let (rest, descriptor) = alt((take_until(" with "), take_until(" and has "))).parse(input)?;
    let (rest, _) = alt((tag(" with "), tag(" and has "))).parse(rest)?;
    let (rest, _) = alt((
        tag("power and toughness each equal to "),
        tag("power and toughness are each equal to "),
        tag("base power and base toughness each equal to "),
        tag("base power and base toughness are each equal to "),
        tag("base power and toughness each equal to "),
        tag("base power and toughness are each equal to "),
    ))
    .parse(rest)?;
    let (rest, qty) = nom_quantity::parse_quantity_ref.parse(rest)?;
    let (rest, _) = opt(tag(".")).parse(rest)?;
    let (rest, _) = eof.parse(rest)?;
    Ok((rest, (descriptor, QuantityExpr::Ref { qty })))
}

fn split_animation_dynamic_pt_clause(text: &str) -> Option<(&str, QuantityExpr)> {
    let lower = text.to_lowercase();
    let (_, (descriptor_lower, value)) = parse_dynamic_pt_clause(lower.as_str()).ok()?;
    let descriptor = text[..descriptor_lower.len()].trim_end_matches(',').trim();
    Some((descriptor, value))
}

/// Classification of a single token within a "becomes [type expression]" noun
/// phrase. Encodes the full design space so callers can't conflate core types
/// (emitted as `AddType`) with subtypes (emitted as `AddSubtype`) or leak
/// supertypes (recognized-but-discarded: animations never change supertypes).
#[derive(Debug, Clone, PartialEq, Eq)]
enum AnimationTypeToken {
    /// CR 205.2a core type — maps to `ContinuousModification::AddType`.
    CoreType(&'static str),
    /// CR 205.3 subtype — maps to `ContinuousModification::AddSubtype`.
    Subtype(String),
    /// CR 205.4 supertype — recognized to avoid halting the sequence, but
    /// not emitted as a modification (animations don't grant supertypes).
    Supertype,
}

/// Zero-width word-boundary check: next char must be non-alphabetic (whitespace,
/// punctuation, or end-of-input). Mirrors the pattern used by `parse_article_number`
/// and `parse_keyword_name` to prevent "land" from swallowing "landwalk".
fn alpha_word_boundary(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        peek(alt((
            nom::combinator::eof,
            recognize(satisfy(|c: char| !c.is_ascii_alphabetic())),
        ))),
    )
    .parse(input)
}

/// Parse a CR 205.2a core type keyword (case-insensitive, word-boundary terminated).
fn parse_animation_core_type(input: &str) -> OracleResult<'_, AnimationTypeToken> {
    let (rest, core) = alt((
        value("Artifact", tag_no_case("artifact")),
        value("Creature", tag_no_case("creature")),
        value("Enchantment", tag_no_case("enchantment")),
        value("Land", tag_no_case("land")),
        value("Planeswalker", tag_no_case("planeswalker")),
    ))
    .parse(input)?;
    // CR 205.2a: accept an optional plural "s" so a plural-subject animation
    // ("All lands are 1/1 creatures that are still lands") recognizes the same
    // core type as the singular "becomes a creature" form. The trailing word
    // boundary still rejects longer words ("landwalk", "creatured").
    let (rest, _) = opt(tag_no_case::<_, _, OracleError<'_>>("s")).parse(rest)?;
    let (rest, _) = alpha_word_boundary(rest)?;
    Ok((rest, AnimationTypeToken::CoreType(core)))
}

/// Parse a CR 205.4 supertype keyword (case-insensitive, word-boundary terminated).
fn parse_animation_supertype(input: &str) -> OracleResult<'_, AnimationTypeToken> {
    let (rest, _) = alt((
        tag_no_case("legendary"),
        tag_no_case("basic"),
        tag_no_case("snow"),
    ))
    .parse(input)?;
    let (rest, _) = alpha_word_boundary(rest)?;
    Ok((rest, AnimationTypeToken::Supertype))
}

/// Parse a CR 205.3 subtype: a capitalized proper-noun word of length ≥ 2,
/// optionally hyphenated (`Power-Plant`, `Lhurgoyf`). Rejects single-letter
/// tokens (`X` in "X/X"), lowercase connectives (`and`, `gets`, `gains`,
/// `until`), and mid-word positions (if followed by `/`, `:`, digits, etc.).
fn parse_animation_subtype(input: &str) -> OracleResult<'_, AnimationTypeToken> {
    let (rest, word) = recognize(pair(
        // First char: capital letter.
        satisfy(|c: char| c.is_ascii_uppercase()),
        // Second char must be alphabetic (no leading-hyphen tokens like "A-B").
        // Subsequent chars may be alphabetic or hyphen (for "Power-Plant").
        pair(
            satisfy(|c: char| c.is_ascii_alphabetic()),
            many0(satisfy(|c: char| c.is_ascii_alphabetic() || c == '-')),
        ),
    ))
    .parse(input)?;
    // Word-boundary: reject follow-ups like `/`, `:`, digits, `{`, `+`, `"` —
    // these indicate we landed mid-P/T-token (`Dragon3/3`) or mid-cost (`B:`).
    let (rest, _) = peek(alt((
        nom::combinator::eof,
        recognize(satisfy(|c: char| {
            c.is_whitespace() || matches!(c, ',' | '.' | ';' | ')' | '!' | '?')
        })),
    )))
    .parse(rest)?;
    Ok((rest, AnimationTypeToken::Subtype(word.to_string())))
}

fn parse_animation_type_token(input: &str) -> OracleResult<'_, AnimationTypeToken> {
    alt((
        parse_animation_core_type,
        parse_animation_supertype,
        parse_animation_subtype,
    ))
    .parse(input)
}

/// Parse a whitespace-separated sequence of type tokens, halting at the first
/// non-type token. Used by [`parse_animation_types`] as the grammar root.
fn parse_animation_type_sequence(input: &str) -> OracleResult<'_, Vec<AnimationTypeToken>> {
    separated_list1(multispace1, parse_animation_type_token).parse(input)
}

/// CR 205.3: Case-insensitive subtype grammar — accepts either a capitalized
/// proper noun (standard form) OR a lowercase alphabetic word (≥ 2 chars,
/// optionally hyphenated). Used only in the "loose" type-sequence parse path
/// which requires the trailing "in addition to its other [creature ]types"
/// structural signal that guarantees the preceding phrase is a type
/// expression (e.g., trigger-effect text that has been pre-lowercased by the
/// oracle_trigger pipeline).
fn parse_animation_subtype_loose(input: &str) -> OracleResult<'_, AnimationTypeToken> {
    let (rest, word) = recognize(pair(
        satisfy(|c: char| c.is_ascii_alphabetic()),
        pair(
            satisfy(|c: char| c.is_ascii_alphabetic()),
            many0(satisfy(|c: char| c.is_ascii_alphabetic() || c == '-')),
        ),
    ))
    .parse(input)?;
    let (rest, _) = peek(alt((
        nom::combinator::eof,
        recognize(satisfy(|c: char| {
            c.is_whitespace() || matches!(c, ',' | '.' | ';' | ')' | '!' | '?')
        })),
    )))
    .parse(rest)?;
    Ok((rest, AnimationTypeToken::Subtype(word.to_string())))
}

/// Case-insensitive type-token parser: core type / supertype / subtype,
/// accepting lowercase subtypes so pre-lowered trigger-effect text (where the
/// CR 205.3 proper-noun casing has been destroyed upstream) can still be
/// decomposed. Halting words are excluded via the terminator arms in
/// [`parse_animation_type_sequence_loose`].
fn parse_animation_type_token_loose(input: &str) -> OracleResult<'_, AnimationTypeToken> {
    alt((
        parse_animation_core_type,
        parse_animation_supertype,
        parse_animation_subtype_loose,
    ))
    .parse(input)
}

fn parse_animation_type_sequence_loose(input: &str) -> OracleResult<'_, Vec<AnimationTypeToken>> {
    separated_list1(multispace1, parse_animation_type_token_loose).parse(input)
}

/// Run the strict (CR 205.3 capitalized) type-sequence parser; fall back to the
/// case-insensitive `_loose` variant when the input is terminated by an
/// "in addition to {its/their/his/her} other [creature ]types" structural signal. The tail
/// guarantees the preceding phrase is a type expression, so lowercase subtype
/// words are safe to classify. Shared by [`parse_becomes_type_modifications`]
/// and [`parse_animation_types`] so both the static-ability and effect-
/// imperative paths decompose the descriptor identically.
fn try_parse_type_sequence_with_suffix(input: &str) -> Option<Vec<AnimationTypeToken>> {
    // Strict path first — preserves existing behavior when the CR 205.3
    // capitalization is present (native effect text, static abilities). The
    // terminator-halt grammar (capitalized subtype required) naturally stops
    // the sequence at lowercase connective words like "in".
    let suffix_parser = opt(preceded(multispace0, parse_in_addition_other_types_marker));
    if let Ok((_, (tokens, _))) = (parse_animation_type_sequence, suffix_parser).parse(input) {
        return Some(tokens);
    }

    // Loose fallback: only fires when the trailing "in addition to its other
    // [creature ]types" marker is present, which structurally guarantees the
    // preceding phrase is a type expression. Because the loose subtype
    // grammar accepts any alphabetic word, we must first split the input on
    // the structural marker so the loose sequence doesn't greedily consume
    // "in addition to its other types" as six subtypes.
    let (prefix, _) = split_in_addition_tail(input)?;
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return None;
    }
    if let Ok((_, tokens)) = parse_animation_type_sequence_loose(prefix) {
        return Some(tokens);
    }

    None
}

/// Split `input` on the " in addition to {its/their/his/her} other
/// [creature ]types" marker, returning the prefix and the matched marker
/// variant. Returns `None` if the marker is absent. Uses `take_until` to
/// locate the marker, then [`parse_in_addition_other_types_marker`] to consume
/// and recognize the variant.
fn split_in_addition_tail(input: &str) -> Option<(&str, &str)> {
    type VE<'a> = OracleError<'a>;
    let (_, prefix) =
        nom::bytes::complete::take_until::<_, _, VE<'_>>(" in addition to ")(input).ok()?;
    let pos = prefix.len();
    let rest = input[pos..].trim_start();
    let (_, matched) = parse_in_addition_other_types_marker(rest).ok()?;
    Some((prefix, matched))
}

/// nom combinator: match the full "in addition to {its/their/his/her} other
/// [creature ]types" marker. The two independent axes — possessive pronoun and
/// type scope (all "types" vs only "creature types") — are composed as a single
/// `alt` and a single `opt`, not enumerated as the 4×2 = 8 `tag` cross product.
/// See `oracle_nom/PATTERNS.md` §8 ("compose, don't enumerate permutations").
/// `recognize` returns the consumed slice, preserving the matched-marker
/// semantics callers rely on. `opt(tag("creature "))` makes the scope axis
/// order-independent — no "longest alternative first" ordering footgun.
fn parse_in_addition_other_types_marker(input: &str) -> OracleResult<'_, &str> {
    recognize((
        tag("in addition to "),
        alt((tag("its"), tag("their"), tag("his"), tag("her"))),
        tag(" other "),
        opt(tag("creature ")),
        tag("types"),
    ))
    .parse(input)
}

/// nom combinator: locate the marker anywhere in `input`, skipping preceding
/// text. Named rather than inlined into [`has_in_addition_to_other_types`] so
/// lifetime elision binds the parser's borrow to the `input` parameter;
/// inlining the combinator over a local `String` tripped E0597 on the
/// temporary parser's drop.
fn locate_in_addition_other_types_marker(input: &str) -> OracleResult<'_, &str> {
    preceded(
        take_until("in addition to "),
        parse_in_addition_other_types_marker,
    )
    .parse(input)
}

pub(crate) fn has_in_addition_to_other_types(text: &str) -> bool {
    let lower = text.to_lowercase();
    locate_in_addition_other_types_marker(&lower).is_ok()
}

/// CR 205.1a + CR 205.2 + CR 205.3 + CR 613.1c: Decompose a "becomes a
/// [subtype]* [core-type]+ [in addition to its other types]?" descriptor into
/// a list of typed `ContinuousModification`s.
///
/// Built on the shared `parse_animation_type_sequence` combinator so callers
/// outside the effect-animation path (e.g., static-ability parsing of
/// "target creature ... becomes a Horror enchantment creature in addition to
/// its other types") get the same type-line decomposition: one `AddType` per
/// CR 205.2 core type, one `AddSubtype` per CR 205.3 subtype, supertypes
/// discarded (CR 205.4 — animations never grant supertypes).
///
/// The descriptor is the noun phrase *after* the "becomes a"/"becomes an"
/// article and *before* any trailing "in addition to its other types" clause.
/// Input must preserve original casing because the CR 205.3 subtype grammar
/// requires capitalized proper nouns.
pub(crate) fn parse_becomes_type_modifications(
    descriptor: &str,
) -> Vec<crate::types::ability::ContinuousModification> {
    use crate::types::ability::ContinuousModification;
    use crate::types::card_type::CoreType;

    let trimmed = descriptor
        .trim()
        .trim_end_matches('.')
        .trim_end_matches(',')
        .trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    // Forward-parse: type-token sequence (halts at first non-classifying word
    // such as "in"), then optionally consume a trailing "in addition to its
    // other [creature] types" clause. The longer alternative is tried first
    // because nom's alt() is short-circuit. See oracle_nom/PATTERNS.md
    // ("Optional trailing clause after a token sequence").
    //
    // When the trailing "in addition to its other [creature] types" signal is
    // present and the strict (capitalized) grammar fails — e.g., trigger-effect
    // text that upstream lowercased before reaching here — retry with the
    // case-insensitive `_loose` variant. The structural tail guarantees the
    // preceding phrase is a type expression, so lowercase subtype words are
    // safe to classify (CR 205.3 applies regardless of glyph case).
    let tokens = match try_parse_type_sequence_with_suffix(trimmed) {
        Some(tokens) => tokens,
        None => return Vec::new(),
    };

    let mut modifications = Vec::new();
    for token in tokens {
        match token {
            AnimationTypeToken::CoreType(name) => {
                if let Ok(ct) = CoreType::from_str(name) {
                    let modification = ContinuousModification::AddType { core_type: ct };
                    if !modifications.contains(&modification) {
                        modifications.push(modification);
                    }
                }
            }
            AnimationTypeToken::Subtype(name) => {
                let modification = ContinuousModification::AddSubtype {
                    subtype: title_case_word(&name),
                };
                if !modifications.contains(&modification) {
                    modifications.push(modification);
                }
            }
            AnimationTypeToken::Supertype => {}
        }
    }
    modifications
}

/// Parse the "becomes a [type expression]" noun phrase into core types +
/// subtypes. Built on nom combinators: tokenizes a sequence of type/subtype
/// words separated by whitespace, halting at the first token that doesn't
/// classify — punctuation (`,`, `.`), lowercase connectives (`and`, `gets`,
/// `gains`, `until`), P/T values (`3/3`, `X/X`), or cost tokens (`{B}:`).
/// This prevents misparses like *"this creature becomes a Dragon, gets +5/+3,
/// and gains flying"* from sweeping `Gets`, `And`, `Gains`, `Flying` in as
/// AddSubtype modifications — a common coverage false-positive pattern.
fn parse_animation_types(text: &str, infer_creature: bool) -> Vec<String> {
    let descriptor = text.trim().trim_end_matches(',').trim();
    if descriptor.is_empty() {
        return Vec::new();
    }

    // See parse_becomes_type_modifications for the same forward-parse pattern.
    // oracle_nom/PATTERNS.md ("Optional trailing clause after a token sequence").
    let tokens = match try_parse_type_sequence_with_suffix(descriptor) {
        Some(tokens) => tokens,
        None => return Vec::new(),
    };

    let mut core_types = Vec::new();
    let mut subtypes = Vec::new();
    for token in tokens {
        match token {
            AnimationTypeToken::CoreType(name) => push_unique_string(&mut core_types, name),
            AnimationTypeToken::Subtype(name) => subtypes.push(title_case_word(&name)),
            AnimationTypeToken::Supertype => {}
        }
    }

    if core_types.is_empty() && subtypes.is_empty() {
        return Vec::new();
    }
    if core_types.is_empty() && infer_creature {
        push_unique_string(&mut core_types, "Creature");
    }

    let mut types = core_types;
    for subtype in subtypes {
        push_unique_string(&mut types, subtype);
    }
    types
}

fn split_animation_keyword_clause(text: &str) -> (&str, Vec<Keyword>) {
    const NEEDLE: &str = " with ";
    let lower = text.to_lowercase();
    let Some((before, _)) = split_around(&lower, NEEDLE) else {
        return (text, Vec::new());
    };

    let pos = before.len();
    let prefix = text[..pos].trim_end_matches(',').trim();
    // allow-noncombinator: structural post-processing of an already-chunked
    // keyword phrase (split at the first `"` above); this is not parsing
    // dispatch. A nom-combinator rewrite would add a word-boundary scan
    // helper without improving correctness.
    let keyword_text = text[pos + NEEDLE.len()..]
        .split('"')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.')
        .trim_end_matches(" in addition to its other types");
    // CR 305.7 + CR 613.1d (#2917): a trailing "that's still a land" rider
    // (Nissa, Who Shakes the World and the land-animation family) confirms the
    // permanent keeps its land type — it is NOT a keyword. Without stripping it
    // the last keyword fuses with the rider ("haste that's still a land") and is
    // dropped by `map_token_keyword`, so e.g. "with vigilance and haste that's
    // still a land" silently lost haste.
    let keyword_text = strip_still_a_land_rider(keyword_text);
    let keywords = split_token_keyword_list(keyword_text)
        .into_iter()
        .filter_map(map_token_keyword)
        .collect();
    (prefix, keywords)
}

/// Cut a trailing "that's/that is/it's/they're still a[n] land[s]" rider off a
/// keyword phrase so it cannot fuse with (and discard) the final keyword.
fn strip_still_a_land_rider(text: &str) -> &str {
    let lower = text.to_lowercase();
    [
        " that's still",
        " that is still",
        " it's still",
        " they're still",
    ]
    .iter()
    .filter_map(|marker| lower.find(marker))
    .min()
    .map_or(text, |idx| text[..idx].trim_end())
}

#[cfg(test)]
mod test_den_bugbear {
    use super::*;

    /// #2917 (CR 305.7 / 613.1d): a trailing "that's still a land" rider must
    /// not truncate the keyword list. Nissa, Who Shakes the World grants the
    /// animated land BOTH vigilance and haste — previously haste fused with the
    /// rider ("haste that's still a land") and was dropped.
    #[test]
    fn animation_keywords_survive_still_a_land_rider() {
        use crate::types::keywords::Keyword;
        let (_, keywords) = split_animation_keyword_clause(
            "a 0/0 Elemental creature with vigilance and haste that's still a land",
        );
        assert!(
            keywords.contains(&Keyword::Vigilance),
            "expected Vigilance, got {keywords:?}"
        );
        assert!(
            keywords.contains(&Keyword::Haste),
            "expected Haste (must not be dropped by the rider), got {keywords:?}"
        );
    }

    #[test]
    fn test_animation_with_quoted_trigger() {
        let text = r#"a 3/2 red Goblin creature with "Whenever this creature attacks, create a 1/1 red Goblin creature token that's tapped and attacking." It's still a land"#;
        let spec = parse_animation_spec(text, &mut ParseContext::default());
        eprintln!("spec = {:?}", spec);
        assert!(spec.is_some(), "animation spec should be Some");
        let spec = spec.unwrap();
        assert_eq!(spec.power, Some(3));
        assert_eq!(spec.toughness, Some(2));
    }

    /// Regression: parse_animation_types must halt at connectives and
    /// punctuation rather than sweeping subsequent words in as subtypes.
    /// Previously a text like "Dragon, gets +5/+3, and gains flying and trample"
    /// produced subtypes ["Dragon", "Gets", "+5/+3", "And", "Gains", "Flying", "Trample"].
    #[test]
    fn animation_types_halts_at_connectives_and_punctuation() {
        assert_eq!(
            parse_animation_types("Dragon", true),
            vec!["Creature", "Dragon"]
        );
        assert_eq!(
            parse_animation_types("artifact creature Golem", false),
            vec!["Artifact", "Creature", "Golem"]
        );

        // Trailing comma on a valid subtype: accept the subtype, stop after.
        assert_eq!(
            parse_animation_types("Dragon, gets +5/+3, and gains flying", true),
            vec!["Creature", "Dragon"]
        );

        // Lowercase word immediately after subtype must terminate parsing.
        assert_eq!(
            parse_animation_types("Golem until end of combat", false),
            vec!["Golem"]
        );

        // P/T tokens and quoted triggers must not become subtypes.
        assert_eq!(
            parse_animation_types("Cat X/X", true),
            vec!["Creature", "Cat"]
        );
        assert_eq!(
            parse_animation_types("Shade and gains \"{B}: This creature gets +1/+1\"", true),
            vec!["Creature", "Shade"],
        );

        // Leading lowercase connective before any subtype → nothing parseable.
        assert_eq!(
            parse_animation_types("in addition to its other types and gains flying", false),
            Vec::<String>::new()
        );
    }

    #[test]
    fn animation_dynamic_pt_equal_to_recipient_mana_value() {
        let spec = parse_animation_spec(
            "an artifact creature with power and toughness each equal to its mana value",
            &mut ParseContext::default(),
        )
        .expect("Karn/Sydri animation phrase should parse");
        assert_eq!(spec.types, vec!["Artifact", "Creature"]);

        let mods = animation_modifications(&spec);
        let expected = crate::types::ability::QuantityExpr::Ref {
            qty: crate::types::ability::QuantityRef::ObjectManaValue {
                scope: crate::types::ability::ObjectScope::Recipient,
            },
        };
        assert!(
            mods.contains(&crate::types::ability::ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Artifact,
            })
        );
        assert!(
            mods.contains(&crate::types::ability::ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Creature,
            })
        );
        assert!(mods.contains(
            &crate::types::ability::ContinuousModification::SetPowerDynamic {
                value: expected.clone(),
            }
        ));
        assert!(mods.contains(
            &crate::types::ability::ContinuousModification::SetToughnessDynamic { value: expected }
        ));
    }

    #[test]
    fn become_subtype_replaces_when_not_additive() {
        // CR 205.1a: "becomes a Frog" replaces creature subtypes — a Human
        // Soldier becomes ONLY a Frog — so the granted subtype is preceded by a
        // RemoveAllSubtypes{Creature} wipe (not appended additively).
        use crate::types::ability::ContinuousModification as CM;
        use crate::types::card_type::SubtypeSet;

        let spec = parse_animation_spec("a green Frog", &mut ParseContext::default())
            .expect("Frog animation should parse");
        let mods = animation_modifications_with_replacement(&spec, false);

        let wipe = mods.iter().position(|m| {
            matches!(
                m,
                CM::RemoveAllSubtypes {
                    set: SubtypeSet::Creature
                }
            )
        });
        let add = mods
            .iter()
            .position(|m| matches!(m, CM::AddSubtype { subtype } if subtype == "Frog"));
        assert!(
            wipe.is_some(),
            "non-additive become must wipe creature subtypes, got {mods:?}"
        );
        assert!(add.is_some(), "expected AddSubtype(Frog), got {mods:?}");
        assert!(
            wipe.unwrap() < add.unwrap(),
            "the RemoveAllSubtypes wipe must precede the granted subtype, got {mods:?}"
        );
    }

    #[test]
    fn become_additive_keeps_existing_subtypes() {
        // CR 205.1b: "in addition to its other types" stays additive — no wipe.
        use crate::types::ability::ContinuousModification as CM;

        let spec = parse_animation_spec("a green Frog", &mut ParseContext::default())
            .expect("Frog animation should parse");
        let mods = animation_modifications_with_replacement(&spec, true);

        assert!(
            !mods
                .iter()
                .any(|m| matches!(m, CM::RemoveAllSubtypes { .. })),
            "additive become must not wipe subtypes, got {mods:?}"
        );
        assert!(
            mods.iter()
                .any(|m| matches!(m, CM::AddSubtype { subtype } if subtype == "Frog")),
            "expected AddSubtype(Frog), got {mods:?}"
        );
    }

    #[test]
    fn become_artifact_creature_stays_additive() {
        // CR 205.1b: "becomes an artifact creature" is additive (gains artifact,
        // stays a creature) — keep AddType, never collapse to SetCardTypes.
        use crate::types::ability::ContinuousModification as CM;
        use crate::types::card_type::CoreType;

        let spec = parse_animation_spec("an artifact creature", &mut ParseContext::default())
            .expect("artifact creature animation should parse");
        let mods = animation_modifications_with_replacement(&spec, false);

        assert!(mods.iter().any(|m| matches!(
            m,
            CM::AddType {
                core_type: CoreType::Artifact
            }
        )));
        assert!(mods.iter().any(|m| matches!(
            m,
            CM::AddType {
                core_type: CoreType::Creature
            }
        )));
        assert!(
            !mods.iter().any(|m| matches!(m, CM::SetCardTypes { .. })),
            "artifact creature must stay additive, got {mods:?}"
        );
    }

    #[test]
    fn animation_base_pt_preserves_trailing_bare_keyword() {
        let spec = parse_animation_spec(
            "a Halfling Scout with base power and toughness 2/3 and lifelink",
            &mut ParseContext::default(),
        )
        .expect("Frodo-style animation phrase should parse");

        let mods = animation_modifications(&spec);
        assert!(
            mods.contains(&crate::types::ability::ContinuousModification::SetPower { value: 2 })
        );
        assert!(mods
            .contains(&crate::types::ability::ContinuousModification::SetToughness { value: 3 }));
        assert!(
            mods.contains(&crate::types::ability::ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Creature,
            })
        );
        assert!(
            mods.contains(&crate::types::ability::ContinuousModification::AddSubtype {
                subtype: "Halfling".to_string(),
            })
        );
        assert!(
            mods.contains(&crate::types::ability::ContinuousModification::AddSubtype {
                subtype: "Scout".to_string(),
            })
        );
        assert!(
            mods.contains(&crate::types::ability::ContinuousModification::AddKeyword {
                keyword: Keyword::Lifelink,
            })
        );
    }

    #[test]
    fn animation_and_has_base_pt_equal_to_recipient_mana_value() {
        let spec = parse_animation_spec(
            "a creature in addition to its other types and has base power and base toughness each equal to its mana value",
            &mut ParseContext::default(),
        )
        .expect("Zur-style animation phrase should parse");
        assert_eq!(spec.types, vec!["Creature"]);

        let mods = animation_modifications(&spec);
        let expected = crate::types::ability::QuantityExpr::Ref {
            qty: crate::types::ability::QuantityRef::ObjectManaValue {
                scope: crate::types::ability::ObjectScope::Recipient,
            },
        };
        assert!(mods.contains(
            &crate::types::ability::ContinuousModification::SetPowerDynamic {
                value: expected.clone(),
            }
        ));
        assert!(mods.contains(
            &crate::types::ability::ContinuousModification::SetToughnessDynamic { value: expected }
        ));
    }

    /// Regression: supertypes (CR 205.4) must be recognized-and-discarded
    /// so they don't halt the sequence. Animations never grant supertypes,
    /// but a leading `legendary` / `basic` / `snow` word in the noun phrase
    /// must not prevent the subtype that follows from being captured.
    #[test]
    fn animation_types_discards_supertypes_without_halting_sequence() {
        assert_eq!(
            parse_animation_types("legendary Angel creature", false),
            vec!["Creature", "Angel"]
        );
        assert_eq!(parse_animation_types("basic Forest", false), vec!["Forest"]);
        // Supertype between core type and subtype must not halt.
        assert_eq!(
            parse_animation_types("snow Creature Elemental", false),
            vec!["Creature", "Elemental"]
        );
    }

    /// Regression: the subtype grammar must reject tokens where a capital
    /// letter is followed directly by a hyphen (`A-B`). Real MTG subtypes
    /// like `Power-Plant` have at least two alphabetic chars before the
    /// hyphen, so tightening the grammar here closes a lexicon-laxness gap.
    #[test]
    fn animation_subtype_rejects_leading_hyphen_tokens() {
        assert!(parse_animation_subtype("A-B").is_err());
        // Valid hyphenated subtype still parses.
        let (_, token) = parse_animation_subtype("Power-Plant").expect("hyphenated subtype");
        assert_eq!(token, AnimationTypeToken::Subtype("Power-Plant".into()));
    }

    /// CR 205.1a + CR 205.2 + CR 205.3 + CR 613.1c: "becomes a ..." descriptor
    /// must decompose into one AddType per core type and one AddSubtype per
    /// canonical subtype, rather than collapsing the whole phrase into a
    /// single AddSubtype string. This guards the Jump Scare pattern and the
    /// general class of compound type grants.
    #[test]
    fn becomes_type_modifications_decomposes_subtype_and_core_types() {
        use crate::types::ability::ContinuousModification;
        use crate::types::card_type::CoreType;

        // Pure core type.
        assert_eq!(
            parse_becomes_type_modifications("creature"),
            vec![ContinuousModification::AddType {
                core_type: CoreType::Creature
            }]
        );

        // Two core types.
        assert_eq!(
            parse_becomes_type_modifications("artifact creature"),
            vec![
                ContinuousModification::AddType {
                    core_type: CoreType::Artifact
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature
                },
            ]
        );

        // Jump Scare: subtype + two core types.
        assert_eq!(
            parse_becomes_type_modifications("Horror enchantment creature"),
            vec![
                ContinuousModification::AddSubtype {
                    subtype: "Horror".into()
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Enchantment
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature
                },
            ]
        );

        // Vehicle is a CR 205.3 artifact subtype, not a core type.
        assert_eq!(
            parse_becomes_type_modifications("Vehicle artifact creature"),
            vec![
                ContinuousModification::AddSubtype {
                    subtype: "Vehicle".into()
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Artifact
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature
                },
            ]
        );

        // Trailing "in addition to its other types" clause is stripped.
        assert_eq!(
            parse_becomes_type_modifications(
                "Horror enchantment creature in addition to its other types"
            ),
            vec![
                ContinuousModification::AddSubtype {
                    subtype: "Horror".into()
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Enchantment
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature
                },
            ]
        );

        // Supertype (CR 205.4) is recognized and discarded.
        assert_eq!(
            parse_becomes_type_modifications("legendary Angel creature"),
            vec![
                ContinuousModification::AddSubtype {
                    subtype: "Angel".into()
                },
                ContinuousModification::AddType {
                    core_type: CoreType::Creature
                },
            ]
        );

        // Empty / malformed input produces no modifications.
        assert!(parse_becomes_type_modifications("").is_empty());
        assert!(parse_becomes_type_modifications("   ").is_empty());
    }

    /// CR 205.3 + CR 613.1c: Case-insensitive fallback — trigger-effect text
    /// that has been pre-lowercased by the upstream `oracle_trigger` pipeline
    /// (which runs `effect_text.to_lowercase()` before dispatch) must still
    /// decompose into typed modifications when the "in addition to its other
    /// types" structural marker guarantees a type expression. Covers the
    /// Clavileño class: "target attacking Vampire that isn't a Demon becomes
    /// a Demon in addition to its other types."
    #[test]
    fn becomes_type_modifications_lowercase_with_in_addition_tail() {
        use crate::types::ability::ContinuousModification;

        assert_eq!(
            parse_becomes_type_modifications("demon in addition to its other types"),
            vec![ContinuousModification::AddSubtype {
                subtype: "Demon".into()
            }]
        );

        // Creature types tail variant.
        assert_eq!(
            parse_becomes_type_modifications("zombie in addition to its other creature types"),
            vec![ContinuousModification::AddSubtype {
                subtype: "Zombie".into()
            }]
        );

        // Without the tail, loose mode must NOT fire — "demon" alone would
        // have been a capitalized subtype in the original CR 205.3 grammar,
        // but lacking the structural signal we must reject lowercase input.
        assert!(parse_becomes_type_modifications("demon").is_empty());

        // parse_animation_types exercises the same fallback.
        assert_eq!(
            parse_animation_types("demon in addition to its other types", false),
            vec!["Demon"]
        );
    }

    /// CR 107.3 + CR 107.3a: "{X}{G}: Until end of turn, ~ becomes an X/X green Hydra creature"
    /// The X/X P/T in a "becomes" predicate must map to CostXPaid (not Variable("X")),
    /// so the animate effect reads cost_x_paid at resolution rather than failing to resolve X.
    /// Covers Lair of the Hydra and future X-cost X/X animation patterns.
    #[test]
    fn animation_spec_x_x_becomes_cost_x_paid() {
        use crate::types::ability::{ContinuousModification, QuantityExpr, QuantityRef};
        use crate::types::card_type::CoreType;

        let spec =
            parse_animation_spec("an X/X green Hydra creature", &mut ParseContext::default())
                .expect("X/X creature-land animate spec must parse");

        // Dynamic P/T must be CostXPaid (not Variable("X")).
        let expected_qty = QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid,
        };
        assert_eq!(
            spec.dynamic_power,
            Some(expected_qty.clone()),
            "dynamic_power must be CostXPaid"
        );
        assert_eq!(
            spec.dynamic_toughness,
            Some(expected_qty),
            "dynamic_toughness must be CostXPaid"
        );
        // Fixed P/T must be None (X/X is dynamic, not fixed).
        assert_eq!(spec.power, None);
        assert_eq!(spec.toughness, None);

        // Type list must include Creature and Hydra.
        assert!(
            spec.types.contains(&"Creature".to_string()),
            "must include Creature"
        );
        assert!(
            spec.types.contains(&"Hydra".to_string()),
            "must include Hydra"
        );

        // animation_modifications must emit SetPowerDynamic, SetToughnessDynamic, AddType(Creature), AddSubtype(Hydra).
        let mods = animation_modifications(&spec);
        assert!(
            mods.contains(&ContinuousModification::SetPowerDynamic {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid
                }
            }),
            "must include SetPowerDynamic(CostXPaid)"
        );
        assert!(
            mods.contains(&ContinuousModification::SetToughnessDynamic {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::CostXPaid
                }
            }),
            "must include SetToughnessDynamic(CostXPaid)"
        );
        assert!(
            mods.contains(&ContinuousModification::AddType {
                core_type: CoreType::Creature
            }),
            "must include AddType(Creature)"
        );
        assert!(
            mods.contains(&ContinuousModification::AddSubtype {
                subtype: "Hydra".to_string()
            }),
            "must include AddSubtype(Hydra)"
        );
    }

    /// CR 107.3a: X/X P/T with no explicit type (bare "X/X creature" with infer_creature=true).
    #[test]
    fn animation_spec_bare_x_x_infers_creature() {
        use crate::types::ability::QuantityRef;

        let spec = parse_animation_spec("an X/X creature", &mut ParseContext::default())
            .expect("bare X/X creature must parse");

        assert!(spec.dynamic_power.is_some(), "dynamic_power must be set");
        assert!(
            spec.dynamic_toughness.is_some(),
            "dynamic_toughness must be set"
        );
        if let Some(crate::types::ability::QuantityExpr::Ref { qty }) = spec.dynamic_power {
            assert_eq!(qty, QuantityRef::CostXPaid, "must be CostXPaid");
        } else {
            panic!("dynamic_power must be Ref(CostXPaid)");
        }
        assert!(
            spec.types.contains(&"Creature".to_string()),
            "must infer Creature"
        );
    }

    /// CR 107.3c: "X/X where X is ~'s power" — X is bound to the source's power,
    /// NOT the cost paid. This pattern must be detected before the X-cost activation
    /// path to avoid the false match that would emit CostXPaid.
    /// Covers Obuun, Mul Daya Ancestor and similar patterns.
    #[test]
    fn animation_spec_x_x_where_x_is_source_power() {
        use crate::types::ability::{ContinuousModification, QuantityExpr, QuantityRef};

        let spec = parse_animation_spec(
            "an X/X Elemental creature, where X is ~'s power",
            &mut ParseContext::default(),
        )
        .expect("X/X where X is ~'s power must parse");

        // Dynamic P/T must be SourcePower (not CostXPaid).
        let expected_qty = QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Source,
            },
        };
        assert_eq!(
            spec.dynamic_power,
            Some(expected_qty.clone()),
            "dynamic_power must be SourcePower"
        );
        assert_eq!(
            spec.dynamic_toughness,
            Some(expected_qty),
            "dynamic_toughness must be SourcePower"
        );
        // Fixed P/T must be None (X/X is dynamic, not fixed).
        assert_eq!(spec.power, None);
        assert_eq!(spec.toughness, None);

        // Type list must include Creature and Elemental.
        assert!(
            spec.types.contains(&"Creature".to_string()),
            "must include Creature, got: {:?}",
            spec.types
        );
        assert!(
            spec.types.contains(&"Elemental".to_string()),
            "must include Elemental, got: {:?}",
            spec.types
        );

        // animation_modifications must emit SetPowerDynamic, SetToughnessDynamic with SourcePower.
        let mods = animation_modifications(&spec);
        assert!(
            mods.contains(&ContinuousModification::SetPowerDynamic {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Source,
                    }
                }
            }),
            "must include SetPowerDynamic(SourcePower)"
        );
        assert!(
            mods.contains(&ContinuousModification::SetToughnessDynamic {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Power {
                        scope: crate::types::ability::ObjectScope::Source,
                    }
                }
            }),
            "must include SetToughnessDynamic(SourcePower)"
        );
    }
}
