use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::{opt, value};
use nom::Parser;
use nom_language::error::VerboseError;

use super::super::oracle_nom::bridge::nom_on_lower;
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_quantity::parse_cda_quantity;
use super::super::oracle_target::parse_type_phrase;
use super::super::oracle_util::{parse_comparison_suffix, TextPair};
use super::counter::normalize_counter_type;
use super::{parse_effect_chain, scan_contains_phrase};
use crate::parser::oracle_warnings::push_warning;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, CastVariantPaid, Comparator, ControllerRef,
    Duration, Effect, FilterProp, QuantityExpr, QuantityRef, StaticCondition, TargetFilter,
    TypeFilter, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::zones::Zone;

pub(crate) fn split_leading_conditional(text: &str) -> Option<(String, String)> {
    let lower = text.to_lowercase();
    if tag::<_, _, VerboseError<&str>>("if ")
        .parse(lower.as_str())
        .is_err()
    {
        return None;
    }

    let mut paren_depth = 0u32;
    let mut in_quotes = false;
    let bytes = text.as_bytes();

    for (idx, ch) in text.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => paren_depth += 1,
            ')' if !in_quotes => paren_depth = paren_depth.saturating_sub(1),
            ',' if !in_quotes && paren_depth == 0 && !is_thousands_separator_comma(bytes, idx) => {
                let condition_text = text[..idx].trim().to_string();
                let rest = text[idx + 1..].trim();
                if !rest.is_empty() {
                    return Some((condition_text, rest.to_string()));
                }
            }
            _ => {}
        }
    }

    None
}

/// True if the comma at `idx` is part of a numeric thousands-separator
/// (digit before, exactly three digits after, no fourth digit). This mirrors
/// the grouping that [`oracle_nom::primitives::parse_digit_number`] consumes,
/// so the conditional splitter does not bisect numeric literals like
/// "1,000" (e.g. A Good Thing's "if you have 1,000 or more life, ...").
fn is_thousands_separator_comma(bytes: &[u8], idx: usize) -> bool {
    // Need at least one preceding digit.
    if idx == 0 || !bytes[idx - 1].is_ascii_digit() {
        return false;
    }
    // Exactly three digits must follow.
    for offset in 1..=3 {
        match bytes.get(idx + offset) {
            Some(b) if b.is_ascii_digit() => {}
            _ => return false,
        }
    }
    // A fourth following digit invalidates the grouping (e.g. "1,0000").
    !matches!(bytes.get(idx + 4), Some(b) if b.is_ascii_digit())
}

pub(super) fn strip_leading_instead(text: &str) -> String {
    let lower = text.to_lowercase();
    if let Some(((), rest)) = nom_on_lower(text, &lower, |input| {
        value((), tag("instead ")).parse(input)
    }) {
        rest.to_string()
    } else {
        text.to_string()
    }
}

pub(super) fn strip_leading_general_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    if let Some((condition_fragment, body)) = split_leading_conditional(text) {
        let condition_lower = condition_fragment.to_lowercase();
        let cond_text = nom_on_lower(&condition_fragment, &condition_lower, |i| {
            value((), tag("if ")).parse(i)
        })
        .map(|((), rest)| rest)
        .unwrap_or(&condition_fragment)
        .trim();

        if let Some(condition) = try_nom_condition_as_ability_condition(cond_text)
            .or_else(|| parse_condition_text(cond_text))
            .or_else(|| parse_control_count_as_ability_condition(cond_text))
        {
            return (Some(condition), body);
        }
    }
    (None, text.to_string())
}

pub(super) fn strip_additional_cost_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();

    if let Some((_, rest)) = nom_on_lower(text, &lower, |i| {
        value((), tag("if the gift wasn't promised, ")).parse(i)
    }) {
        return (
            Some(AbilityCondition::AdditionalCostNotPaid),
            rest.to_string(),
        );
    }

    if alt((tag::<_, _, VerboseError<&str>>("if "), tag("then if ")))
        .parse(lower.as_str())
        .is_ok()
    {
        if let Ok((_, (_, rest))) =
            nom_primitives::split_once_on(lower.as_str(), " wasn't kicked, ")
                .or_else(|_| nom_primitives::split_once_on(lower.as_str(), " wasn't bargained, "))
        {
            let offset = text.len() - rest.len();
            return (
                Some(AbilityCondition::AdditionalCostNotPaid),
                text[offset..].to_string(),
            );
        }
    }

    let body = if let Some(((), rest)) = nom_on_lower(text, &lower, |input| {
        value(
            (),
            alt((
                tag("if this spell's additional cost was paid, "),
                tag("if evidence was collected, "),
                tag("if the gift was promised, "),
            )),
        )
        .parse(input)
    }) {
        Some(rest.to_string())
    } else if tag::<_, _, VerboseError<&str>>("if ")
        .parse(lower.as_str())
        .is_ok()
    {
        nom_primitives::split_once_on(lower.as_str(), " was kicked, ")
            .or_else(|_| nom_primitives::split_once_on(lower.as_str(), " was bargained, "))
            .ok()
            .map(|(_, (_, rest))| {
                let offset = text.len() - rest.len();
                text[offset..].to_string()
            })
    } else {
        None
    };

    let tp = TextPair::new(text, &lower);
    if body.is_none() && scan_contains_phrase(&lower, "sneak cost was paid") {
        if let Some(after) = tp.strip_after("instead ") {
            return (
                Some(AbilityCondition::CastVariantPaidInstead {
                    variant: CastVariantPaid::Sneak,
                }),
                after.original.to_string(),
            );
        }
        // CR 702.190a: "if this spell's sneak cost was paid, [effect]" — non-"instead"
        // variant that gates a sub-ability on sneak payment.
        if let Some(after) = tp.strip_after("sneak cost was paid, ") {
            return (
                Some(AbilityCondition::CastVariantPaid {
                    variant: CastVariantPaid::Sneak,
                }),
                after.original.to_string(),
            );
        }
    }
    if body.is_none() && scan_contains_phrase(&lower, "ninjutsu cost was paid") {
        if let Some(after) = tp.strip_after("instead ") {
            return (
                Some(AbilityCondition::CastVariantPaidInstead {
                    variant: CastVariantPaid::Ninjutsu,
                }),
                after.original.to_string(),
            );
        }
        // CR 702.49: "if its ninjutsu cost was paid, [effect]" — non-"instead"
        // variant that gates a sub-ability on ninjutsu payment.
        if let Some(after) = tp.strip_after("ninjutsu cost was paid, ") {
            return (
                Some(AbilityCondition::CastVariantPaid {
                    variant: CastVariantPaid::Ninjutsu,
                }),
                after.original.to_string(),
            );
        }
    }

    match body {
        Some(body) => {
            let body_lower = body.to_lowercase();
            let (body, condition) = if let Some(stripped) = body_lower
                .strip_suffix(" instead")
                .map(|_| &body[..body.len() - " instead".len()])
            {
                (
                    stripped.to_string(),
                    AbilityCondition::AdditionalCostPaidInstead,
                )
            } else {
                let stripped = strip_leading_instead(&body);
                if stripped.len() < body.len() {
                    (stripped, AbilityCondition::AdditionalCostPaidInstead)
                } else {
                    (body, AbilityCondition::AdditionalCostPaid)
                }
            };
            (Some(condition), body)
        }
        None => (None, text.to_string()),
    }
}

pub(super) fn strip_if_you_do_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();

    if let Some((condition, rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(AbilityCondition::WhenYouDo, tag("when you do, ")),
            value(AbilityCondition::IfAPlayerDoes, tag("if a player does, ")),
            value(AbilityCondition::IfAPlayerDoes, tag("if they do, ")),
            value(AbilityCondition::IfYouDo, tag("if you do, ")),
        ))
        .parse(input)
    }) {
        return (Some(condition), rest.to_string());
    }

    if let Some(after_article) = {
        let result: Option<&str> = alt((tag::<_, _, VerboseError<&str>>("if a "), tag("if an ")))
            .parse(lower.as_str())
            .ok()
            .map(|(rest, _)| rest);
        result
    } {
        if let Some((noun_phrase, after_was)) = after_article.split_once(" was ") {
            let mut words = after_was.splitn(3, ' ');
            if let (Some(_verb), Some("this"), Some(rest_with_way)) =
                (words.next(), words.next(), words.next())
            {
                if let Some(body) = rest_with_way.strip_prefix("way, ") {
                    let (filter, _) = parse_type_phrase(noun_phrase);
                    let offset = text.len() - body.len();
                    return (
                        Some(AbilityCondition::ZoneChangedThisWay { filter }),
                        text[offset..].to_string(),
                    );
                }
            }
        }
    }
    (None, text.to_string())
}

pub(super) fn strip_unless_entered_suffix(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    for pattern in &[
        "unless ~ entered this turn",
        "unless this creature entered this turn",
    ] {
        if let Some((before, _)) = tp.split_around(pattern) {
            return (
                Some(AbilityCondition::SourceDidNotEnterThisTurn),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }
    if let Some((effect_part, condition_part)) = lower.rsplit_once(" unless ") {
        let condition_text = condition_part.trim_end_matches('.');
        if let Some(cond) = try_nom_condition_as_unless(condition_text) {
            let effect_text = text[..effect_part.len()].trim().to_string();
            return (Some(cond), effect_text);
        }
    }
    (None, text.to_string())
}

fn try_nom_condition_as_unless(condition_text: &str) -> Option<AbilityCondition> {
    use crate::parser::oracle_nom::condition::parse_inner_condition;

    let (rest, inner) = parse_inner_condition(condition_text).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    let negated = StaticCondition::Not {
        condition: Box::new(inner),
    };
    static_condition_to_ability_condition(&negated)
}

pub(super) fn strip_cast_from_zone_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    if let Some((zone, rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(Zone::Hand, tag("if you cast it from your hand")),
            value(Zone::Exile, tag("if you cast it from exile")),
            value(Zone::Graveyard, tag("if you cast it from your graveyard")),
        ))
        .parse(input)
    }) {
        let rest = rest.strip_prefix(", ").unwrap_or(rest);
        return (
            Some(AbilityCondition::CastFromZone { zone }),
            rest.to_string(),
        );
    }
    (None, text.to_string())
}

pub(super) fn strip_card_type_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let rest = alt((
        tag::<_, _, VerboseError<&str>>("if it's a "),
        tag("if it's an "),
    ))
    .parse(lower.as_str())
    .ok()
    .map(|(rest, _)| rest);
    let Some(rest) = rest else {
        return (None, text.to_string());
    };
    let (rest, negated) = opt(tag::<_, _, VerboseError<&str>>("non"))
        .parse(rest)
        .map(|(rest, matched)| (rest, matched.is_some()))
        .unwrap_or((rest, false));
    let (type_str, after_type) = if let Some(type_end) = rest.find(" card") {
        (&rest[..type_end], &rest[type_end + " card".len()..])
    } else if let Some(comma_pos) = rest.find(", ") {
        (&rest[..comma_pos], &rest[comma_pos..])
    } else {
        return (None, text.to_string());
    };
    let type_word = type_str.rsplit(' ').next().unwrap_or(type_str);
    let capitalized = format!("{}{}", &type_word[..1].to_uppercase(), &type_word[1..]);
    if let Ok(card_type) = CoreType::from_str(&capitalized) {
        // CR 205.3m: Consume optional "of the chosen type" suffix after " card".
        let (after_type, additional_filter) = if let Ok((rest_after_chosen, _)) =
            tag::<_, _, VerboseError<&str>>(" of the chosen type").parse(after_type)
        {
            (rest_after_chosen, Some(FilterProp::IsChosenCreatureType))
        } else {
            (after_type, None)
        };
        let remainder = after_type.strip_prefix(", ").unwrap_or(after_type);
        let offset = text.len() - remainder.len();
        return (
            Some(AbilityCondition::RevealedHasCardType {
                card_type,
                negated,
                additional_filter,
            }),
            text[offset..].to_string(),
        );
    }
    (None, text.to_string())
}

fn parse_its_a_type_condition(condition_text: &str) -> Option<AbilityCondition> {
    let (rest, _) = alt((tag::<_, _, VerboseError<&str>>("it's a "), tag("it's an ")))
        .parse(condition_text)
        .ok()?;
    let (rest, negated) = opt(tag::<_, _, VerboseError<&str>>("non"))
        .parse(rest)
        .map(|(rest, matched)| (rest, matched.is_some()))
        .unwrap_or((rest, false));
    let type_str = rest
        .strip_suffix(" card")
        .unwrap_or(rest)
        .trim_end_matches('.');
    let type_word = type_str.rsplit(' ').next().unwrap_or(type_str);
    let capitalized = format!("{}{}", &type_word[..1].to_uppercase(), &type_word[1..]);
    let card_type = CoreType::from_str(&capitalized).ok()?;
    Some(AbilityCondition::RevealedHasCardType {
        card_type,
        negated,
        additional_filter: None,
    })
}

pub(super) fn try_parse_type_setting(text: &str) -> Option<AbilityDefinition> {
    let lower = text.to_lowercase();
    let lower = lower.trim_end_matches('.');

    let (type_name, _) = alt((tag::<_, _, VerboseError<&str>>("it's a "), tag("it's an ")))
        .parse(lower)
        .ok()?;

    let type_name = type_name.trim();
    let capitalized = format!("{}{}", &type_name[..1].to_uppercase(), &type_name[1..]);
    CoreType::from_str(&capitalized).ok()?;

    let mut remove_types = Vec::new();
    if capitalized != "Creature" {
        remove_types.push("Creature".to_string());
    }

    let effect = Effect::Animate {
        power: None,
        toughness: None,
        types: vec![capitalized],
        remove_types,
        target: TargetFilter::None,
        keywords: vec![],
    };

    let mut def = AbilityDefinition::new(AbilityKind::Spell, effect);
    def = def.duration(Duration::Permanent);
    Some(def)
}

pub(super) fn strip_turn_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    if let Some((negated, rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(false, tag("if it's your turn, ")),
            value(true, tag("if it's not your turn, ")),
            value(true, tag("if it isn't your turn, ")),
        ))
        .parse(input)
    }) {
        return (
            Some(AbilityCondition::IsYourTurn { negated }),
            rest.to_string(),
        );
    }
    (None, text.to_string())
}

pub(super) fn strip_property_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    for (property, qty_ref) in &[
        ("power", QuantityRef::EventContextSourcePower),
        ("toughness", QuantityRef::EventContextSourceToughness),
    ] {
        let pattern = format!(" if its {property} is ");
        if let Some((before, after)) = tp.rsplit_around(&pattern) {
            let after = after.lower.trim_end_matches('.');

            if let Some((comparator, value)) = parse_comparison_suffix(after) {
                return (
                    Some(AbilityCondition::QuantityCheck {
                        lhs: QuantityExpr::Ref {
                            qty: qty_ref.clone(),
                        },
                        comparator,
                        rhs: QuantityExpr::Fixed { value },
                    }),
                    before.original.to_string(),
                );
            }
        }
    }

    for (pattern, use_lki) in &[
        (" if that creature was a ", true),
        (" if that creature was an ", true),
        (" if that creature is a ", false),
        (" if that creature is an ", false),
    ] {
        if let Some((before, after)) = tp.rsplit_around(pattern) {
            let type_text = after.lower.trim_end_matches('.').trim();
            let (filter, leftover) = parse_type_phrase(type_text);
            if !matches!(filter, TargetFilter::Any) && leftover.trim().is_empty() {
                return (
                    Some(AbilityCondition::TargetMatchesFilter {
                        filter,
                        use_lki: *use_lki,
                        negated: false,
                    }),
                    before.original.to_string(),
                );
            }
        }
    }

    (None, text.to_string())
}

pub(super) fn strip_target_keyword_instead(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let prefix = alt((
        tag::<_, _, VerboseError<&str>>("if that creature has "),
        tag("if that permanent has "),
    ))
    .parse(lower.as_str())
    .ok()
    .map(|(rest, _)| rest);
    if let Some(rest) = prefix {
        if let Some((keyword_str, body)) = rest.split_once(", ") {
            let keyword = crate::types::keywords::Keyword::from_str(keyword_str.trim()).unwrap();
            let body = body.trim();
            let body_text = text[text.len() - body.len()..].trim();
            let body_text = body_text
                .strip_suffix(" instead.")
                .or_else(|| body_text.strip_suffix(" instead"))
                .unwrap_or(body_text);
            let body_text = body_text.strip_prefix("it ").unwrap_or(body_text);
            return (
                Some(AbilityCondition::TargetHasKeywordInstead { keyword }),
                body_text.to_string(),
            );
        }
    }
    (None, text.to_string())
}

fn parse_counter_threshold(text: &str) -> Option<(Comparator, i32, String, usize)> {
    let original_len = text.len();

    fn parse_counter_on_suffix(after_type: &str) -> Option<&str> {
        let (after_counter, _) = alt((tag::<_, _, VerboseError<&str>>("counters"), tag("counter")))
            .parse(after_type)
            .ok()?;
        let (after_on, _) = alt((tag::<_, _, VerboseError<&str>>("on it"), tag("on this")))
            .parse(after_counter.trim_start())
            .ok()?;
        Some(after_on)
    }

    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("no ").parse(text) {
        let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let raw_type = &rest[..type_end];
        let counter_type = normalize_counter_type(raw_type);
        let after_type = rest[type_end..].trim_start();
        let after_on = parse_counter_on_suffix(after_type)?;
        let consumed = original_len - after_on.len();
        return Some((Comparator::EQ, 0, counter_type, consumed));
    }

    let (rest, threshold) = nom_primitives::parse_number.parse(text).ok()?;
    let rest = rest.trim_start();
    type E<'a> = VerboseError<&'a str>;
    let (rest, comparator) = alt((
        value(Comparator::GE, tag::<_, _, E>("or more ")),
        value(Comparator::LE, tag("or fewer ")),
    ))
    .parse(rest)
    .ok()?;

    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let raw_type = &rest[..type_end];
    let counter_type = normalize_counter_type(raw_type);
    let after_type = rest[type_end..].trim_start();
    let after_on = parse_counter_on_suffix(after_type)?;
    let consumed = original_len - after_on.len();
    Some((comparator, threshold as i32, counter_type, consumed))
}

fn build_counter_condition(
    comparator: Comparator,
    threshold: i32,
    counter_type: String,
) -> AbilityCondition {
    AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::CountersOnSelf { counter_type },
        },
        comparator,
        rhs: QuantityExpr::Fixed { value: threshold },
    }
}

pub(super) fn strip_counter_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("if it has ").parse(lower.as_str()) {
        if let Some((comparator, threshold, counter_type, consumed)) = parse_counter_threshold(rest)
        {
            let after = rest[consumed..].trim_start();
            let after = after.strip_prefix(',').unwrap_or(after).trim_start();
            let offset = text.len() - after.len();
            return (
                Some(build_counter_condition(comparator, threshold, counter_type)),
                text[offset..].to_string(),
            );
        }
    }

    if let Some((before, after)) = tp.rsplit_around(" if it has ") {
        if let Some((comparator, threshold, counter_type, consumed)) =
            parse_counter_threshold(after.lower)
        {
            let remaining = after.lower[consumed..].trim();
            if remaining.is_empty() || remaining == "." {
                return (
                    Some(build_counter_condition(comparator, threshold, counter_type)),
                    before.original.trim_end_matches('.').trim().to_string(),
                );
            }
        }
    }

    (None, text.to_string())
}

/// CR 202.3 + CR 608.2c: Strip trailing "if it has mana value N or less/greater" from
/// effect text. Returns a `TargetMatchesFilter` condition with `CmcLE`/`CmcGE` property.
/// Handles the class of cards that conditionally apply effects based on target mana value
/// (Fatal Push, Anoint with Affliction, Angrath, Cosmic Rebirth, etc.).
pub(super) fn strip_mana_value_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Suffix position: "[effect] if it has mana value N or less/greater."
    if let Some((before, after)) = tp.rsplit_around(" if it has mana value ") {
        if let Some((comparator, threshold)) = parse_mana_value_threshold(after.lower) {
            let prop = match comparator {
                Comparator::LE => FilterProp::CmcLE {
                    value: QuantityExpr::Fixed { value: threshold },
                },
                Comparator::GE => FilterProp::CmcGE {
                    value: QuantityExpr::Fixed { value: threshold },
                },
                _ => return (None, text.to_string()),
            };
            let condition = AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![prop])),
                use_lki: false,
                negated: false,
            };
            return (
                Some(condition),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }

    (None, text.to_string())
}

/// Parse "N or less" / "N or greater" from mana value threshold text.
/// Uses nom combinators to extract the numeric threshold and comparison direction.
fn parse_mana_value_threshold(text: &str) -> Option<(Comparator, i32)> {
    let text = text.trim().trim_end_matches('.');
    // Parse: number + " or " + "less"/"greater"
    let (rest, n) = nom_primitives::parse_number(text).ok()?;
    let (rest, _) = tag::<_, _, VerboseError<&str>>(" or ").parse(rest).ok()?;
    let (_, comparator) = alt((
        value(Comparator::LE, tag::<_, _, VerboseError<&str>>("less")),
        value(Comparator::GE, tag("greater")),
    ))
    .parse(rest)
    .ok()?;
    Some((comparator, n as i32))
}

fn find_last_top_level_if(text: &str) -> Option<usize> {
    let mut last_pos = None;
    let mut paren_depth = 0u32;
    let mut in_quotes = false;

    for (index, ch) in text.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => paren_depth += 1,
            ')' if !in_quotes => paren_depth = paren_depth.saturating_sub(1),
            _ if !in_quotes && paren_depth == 0 && text[index..].starts_with(" if ") => {
                last_pos = Some(index);
            }
            _ => {}
        }
    }
    last_pos
}

pub(super) fn strip_suffix_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let Some(if_pos) = find_last_top_level_if(&lower) else {
        return (None, text.to_string());
    };

    let condition_text = lower[if_pos + " if ".len()..].trim_end_matches('.').trim();
    let excluded_prefixes = [
        "able",
        "you do",
        "they do",
        "a player does",
        "no one does",
        "no player does",
        "possible",
        "it has ",
        "its power is ",
        "its toughness is ",
        "that creature has ",
        "that permanent has ",
        "you cast it from",
    ];
    for prefix in &excluded_prefixes {
        if condition_text.starts_with(prefix) {
            return (None, text.to_string());
        }
    }

    if let Some(cond) = parse_its_a_type_condition(condition_text) {
        let effect_text = text[..if_pos].trim().to_string();
        return (Some(cond), effect_text);
    }

    if let Some(condition) = try_nom_condition_as_ability_condition(condition_text)
        .or_else(|| parse_condition_text(condition_text))
        .or_else(|| parse_control_count_as_ability_condition(condition_text))
    {
        let effect_text = text[..if_pos].trim().to_string();
        return (Some(condition), effect_text);
    }

    (None, text.to_string())
}

pub(super) fn parse_quantity_comparison(text: &str) -> Option<(Comparator, QuantityExpr)> {
    type E<'a> = VerboseError<&'a str>;
    let mut comparator_prefixes = alt((
        value(Comparator::GE, tag::<_, _, E>("greater than or equal to ")),
        value(Comparator::LE, tag("less than or equal to ")),
        value(Comparator::GT, tag("greater than ")),
        value(Comparator::LT, tag("less than ")),
        value(Comparator::EQ, tag("equal to ")),
    ));

    if let Ok((rhs_text, comparator)) = comparator_prefixes.parse(text) {
        if let Some(rhs) = parse_cda_quantity(rhs_text) {
            return Some((comparator, rhs));
        }
    }
    if let Some((comparator, value)) = parse_comparison_suffix(text) {
        return Some((comparator, QuantityExpr::Fixed { value }));
    }
    None
}

pub(super) fn parse_condition_text(text: &str) -> Option<AbilityCondition> {
    let text = text.trim().trim_end_matches('.');
    let (lhs_text, comparator_rhs) = text.split_once(" is ")?;
    let lhs = parse_cda_quantity(lhs_text)?;
    let (comparator, rhs) = parse_quantity_comparison(comparator_rhs)?;
    Some(AbilityCondition::QuantityCheck {
        lhs,
        comparator,
        rhs,
    })
}

pub(super) fn try_parse_generic_instead_clause(
    text: &str,
    kind: AbilityKind,
) -> Option<AbilityDefinition> {
    let (condition_fragment, raw_body) = split_leading_conditional(text)?;
    let condition_lower = condition_fragment.to_lowercase();
    let cond_text = nom_on_lower(&condition_fragment, &condition_lower, |i| {
        value((), tag("if ")).parse(i)
    })
    .map(|((), rest)| rest)
    .unwrap_or(&condition_fragment)
    .trim();

    let trimmed_body = raw_body.trim_end_matches('.').trim();
    let trimmed_lower = trimmed_body.to_lowercase();
    let effect_text = if let Some(stripped) = trimmed_body.strip_suffix(" instead") {
        stripped.trim()
    } else if let Some((_, rest)) = nom_on_lower(trimmed_body, &trimmed_lower, |i| {
        value((), tag("instead ")).parse(i)
    }) {
        rest.trim()
    } else {
        return None;
    };

    let condition = try_nom_condition_as_ability_condition(cond_text)
        .or_else(|| parse_condition_text(cond_text))
        .or_else(|| parse_control_count_as_ability_condition(cond_text))?;

    let instead_def = parse_effect_chain(effect_text, kind);
    let mut result = instead_def;
    result.condition = Some(condition);
    Some(result)
}

/// CR 608.2c: "If <cond>, you may instead <reveal-N-from-among-body>" — conditional
/// alternative selection for a preceding `Effect::Dig`. The "instead" body re-uses
/// the preceding Dig's source (top N cards) but swaps keep_count/up_to/filter/destination.
///
/// Handles patterns like Follow the Lumarets:
///   "Look at the top four cards of your library. You may reveal a creature or land
///    card from among them and put it into your hand. If you gained life this turn,
///    you may instead reveal two creature and/or land cards from among them and put
///    them into your hand."
///
/// Returns a new AbilityDefinition carrying the alternative Dig plus condition; the
/// caller wraps the preceding Dig as `else_ability`. Class coverage: any card of form
/// "look at top N / reveal a <filter> card from among them ... if <cond>, you may
/// instead reveal M <filter'> cards from among them" (CR 608.2c replacement effect).
pub(super) fn try_parse_dig_instead_alternative(
    text: &str,
    previous: Option<&AbilityDefinition>,
    kind: AbilityKind,
) -> Option<AbilityDefinition> {
    use super::sequence::parse_dig_from_among;
    use super::types::ContinuationAst;

    // Gate: previous effect must be a Dig that the alternative can piggy-back on.
    let prev = previous?;
    let Effect::Dig {
        count: prev_count,
        destination: _,
        keep_count: _,
        up_to: _,
        filter: _,
        rest_destination: prev_rest,
        reveal: prev_reveal,
    } = &*prev.effect
    else {
        return None;
    };

    let (condition_fragment, raw_body) = split_leading_conditional(text)?;
    let condition_lower = condition_fragment.to_lowercase();
    let cond_text = nom_on_lower(&condition_fragment, &condition_lower, |i| {
        value((), tag("if ")).parse(i)
    })
    .map(|((), rest)| rest)
    .unwrap_or(&condition_fragment)
    .trim();

    // Strip "you may instead " / "instead " / "you may " from the body to get
    // the bare reveal-from-among clause. Composed with nom combinators; the
    // "you may instead" arm is first so it wins over "you may ".
    let trimmed_body = raw_body.trim_end_matches('.').trim();
    let body_lower = trimmed_body.to_lowercase();
    let ((), body_rest) = nom_on_lower(trimmed_body, &body_lower, |i| {
        value(
            (),
            alt((tag("you may instead "), tag("instead "), tag("you may "))),
        )
        .parse(i)
    })?;

    let body_rest_lower = body_rest.to_lowercase();
    let alt_continuation = parse_dig_from_among(&body_rest_lower, body_rest)?;
    let ContinuationAst::DigFromAmong {
        count: alt_keep_count,
        up_to: alt_up_to,
        filter: alt_filter,
        destination: alt_destination,
        rest_destination: alt_rest,
    } = alt_continuation
    else {
        return None;
    };

    let condition = try_nom_condition_as_ability_condition(cond_text)
        .or_else(|| parse_condition_text(cond_text))
        .or_else(|| parse_control_count_as_ability_condition(cond_text))?;

    // Clone the preceding Dig's source (top N) and reveal-mode, apply alternative
    // selection parameters. `rest_destination` prefers the alternative's inline value
    // (same-clause "and the rest on the bottom..."); otherwise falls back to the
    // preceding Dig's (already-patched or None — a trailing PutRest continuation
    // patches both branches by rewriting into the chain).
    let alt_effect = Effect::Dig {
        count: prev_count.clone(),
        destination: alt_destination,
        keep_count: Some(alt_keep_count),
        up_to: alt_up_to,
        filter: alt_filter,
        rest_destination: alt_rest.or(*prev_rest),
        reveal: *prev_reveal,
    };

    let mut result = AbilityDefinition::new(kind, alt_effect);
    result.condition = Some(condition);
    Some(result)
}

fn parse_control_count_as_ability_condition(text: &str) -> Option<AbilityCondition> {
    let text = text.trim();
    let (rest, _) = tag::<_, _, VerboseError<&str>>("you control ")
        .parse(text)
        .ok()?;

    let (type_rest, _) = tag::<_, _, VerboseError<&str>>("fewer ").parse(rest).ok()?;
    let pos = type_rest.find(" than ")?;
    let type_text = &type_rest[..pos];
    let (mut filter, leftover) = parse_type_phrase(type_text);
    if filter == TargetFilter::Any || !leftover.trim().is_empty() {
        return None;
    }
    if let TargetFilter::Typed(ref mut typed) = filter {
        typed.controller = Some(ControllerRef::You);
    }
    let mut opponent_filter = filter.clone();
    if let TargetFilter::Typed(ref mut typed) = opponent_filter {
        typed.controller = Some(ControllerRef::Opponent);
    }
    Some(AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        },
        comparator: Comparator::LT,
        rhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: opponent_filter,
            },
        },
    })
}

fn static_condition_to_ability_condition(sc: &StaticCondition) -> Option<AbilityCondition> {
    match sc {
        StaticCondition::DuringYourTurn => Some(AbilityCondition::IsYourTurn { negated: false }),
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => Some(AbilityCondition::QuantityCheck {
            lhs: lhs.clone(),
            comparator: *comparator,
            rhs: rhs.clone(),
        }),
        StaticCondition::HasMaxSpeed => Some(AbilityCondition::HasMaxSpeed),
        StaticCondition::SourceEnteredThisTurn => None,
        StaticCondition::IsPresent { filter } => {
            let filter = filter.clone().unwrap_or_else(|| {
                push_warning(
                    "bare-filter: IsPresent condition has no filter, defaulting to Any".to_string(),
                );
                TargetFilter::Any
            });
            Some(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { filter },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        }
        StaticCondition::Not { condition } => match condition.as_ref() {
            StaticCondition::DuringYourTurn => Some(AbilityCondition::IsYourTurn { negated: true }),
            StaticCondition::SourceEnteredThisTurn => {
                Some(AbilityCondition::SourceDidNotEnterThisTurn)
            }
            StaticCondition::QuantityComparison {
                lhs,
                comparator,
                rhs,
            } => Some(AbilityCondition::QuantityCheck {
                lhs: lhs.clone(),
                comparator: comparator.negate(),
                rhs: rhs.clone(),
            }),
            StaticCondition::IsPresent { filter } => {
                let filter = filter.clone().unwrap_or_else(|| {
                    push_warning(
                        "bare-filter: NegatedIsPresent has no filter, defaulting to Any"
                            .to_string(),
                    );
                    TargetFilter::Any
                });
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    },
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 0 },
                })
            }
            // CR 611.2b: Not(SourceIsTapped) → source is untapped.
            StaticCondition::SourceIsTapped => {
                Some(AbilityCondition::SourceIsTapped { negated: true })
            }
            _ => None,
        },
        StaticCondition::SourceMatchesFilter { filter } => {
            Some(AbilityCondition::SourceMatchesFilter {
                filter: filter.clone(),
            })
        }
        StaticCondition::SourceIsTapped => {
            Some(AbilityCondition::SourceIsTapped { negated: false })
        }
        StaticCondition::DevotionGE { .. }
        | StaticCondition::ChosenColorIs { .. }
        | StaticCondition::SpeedGE { .. }
        | StaticCondition::HasCounters { .. }
        | StaticCondition::ClassLevelGE { .. }
        | StaticCondition::IsRingBearer
        | StaticCondition::SourceInZone { .. }
        | StaticCondition::DefendingPlayerControls { .. }
        | StaticCondition::SourceAttackingAlone
        | StaticCondition::SourceIsAttacking
        | StaticCondition::SourceIsBlocking
        | StaticCondition::SourceIsBlocked
        | StaticCondition::SourceIsEquipped
        | StaticCondition::SourceIsMonstrous
        | StaticCondition::SourceAttachedToCreature
        | StaticCondition::IsMonarch
        | StaticCondition::HasCityBlessing
        | StaticCondition::OpponentPoisonAtLeast { .. }
        | StaticCondition::UnlessPay { .. }
        | StaticCondition::Unrecognized { .. }
        | StaticCondition::And { .. }
        | StaticCondition::Or { .. }
        | StaticCondition::RingLevelAtLeast { .. }
        | StaticCondition::CompletedADungeon
        | StaticCondition::ControlsCommander
        | StaticCondition::EnchantedIsFaceDown
        | StaticCondition::SourceControllerEquals { .. }
        | StaticCondition::None => None,
    }
}

pub(super) fn try_nom_condition_as_ability_condition(text: &str) -> Option<AbilityCondition> {
    use crate::parser::oracle_nom::condition::parse_inner_condition;

    let lower = text.to_lowercase();

    // CR 730.2a: "it's neither day nor night" — Daybound/Nightbound ETB initialization.
    if tag::<_, _, VerboseError<&str>>("it's neither day nor night")
        .parse(lower.as_str())
        .is_ok()
    {
        return Some(AbilityCondition::DayNightIsNeither);
    }

    if tag::<_, _, VerboseError<&str>>("you win the clash")
        .parse(lower.as_str())
        .is_ok()
        || tag::<_, _, VerboseError<&str>>("you won the clash")
            .parse(lower.as_str())
            .is_ok()
    {
        return Some(AbilityCondition::IfYouDo);
    }

    if let Ok((rest, _)) =
        tag::<_, _, VerboseError<&str>>("this spell was cast from ").parse(lower.as_str())
    {
        let zone = match rest.trim() {
            "your hand" | "hand" => Some(Zone::Hand),
            "your graveyard" | "a graveyard" => Some(Zone::Graveyard),
            "exile" => Some(Zone::Exile),
            _ => None,
        };
        if let Some(zone) = zone {
            return Some(AbilityCondition::CastFromZone { zone });
        }
    }

    // CR 400.7 + CR 608.2c: "a[n] [type] was [verb]'d this way" — references the
    // LKI of the parent target (the object acted on by the preceding effect).
    // Shredder's Technique: "If an enchantment was destroyed this way, you lose 2 life."
    // "this way" here is scoped to the single parent target of the preceding
    // imperative (Destroy target creature or enchantment). Type-resolution via
    // LKI mirrors the "it was a [type] card" branch below.
    if let Some((type_filter, negated)) = parse_a_type_was_verbed_this_way(&lower) {
        return Some(AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Typed(TypedFilter::new(type_filter)),
            use_lki: true,
            negated,
        });
    }

    // CR 400.7 + CR 608.2c: Past-tense "it was a [type] card" — the card has already
    // moved zones; check its last-known information via TargetMatchesFilter { use_lki }.
    // Distinct from present-tense "it's a [type]" which uses RevealedHasCardType.
    {
        let mut lki_prefix = alt((
            value(true, tag::<_, _, VerboseError<&str>>("it was not a ")),
            value(true, tag("it wasn't a ")),
            value(false, tag("it was a ")),
            value(false, tag("it was an ")),
        ));
        if let Ok((rest, negated_lki)) = lki_prefix.parse(lower.as_str()) {
            // Strip trailing " card" / " card." before delegating to parse_type_phrase.
            let type_text = rest
                .trim_end_matches('.')
                .trim()
                .trim_end_matches(" card")
                .trim();
            let (filter, leftover) = crate::parser::oracle_target::parse_type_phrase(type_text);
            if !matches!(filter, TargetFilter::Any) && leftover.trim().is_empty() {
                return Some(AbilityCondition::TargetMatchesFilter {
                    filter,
                    use_lki: true,
                    negated: negated_lki,
                });
            }
        }
    }

    let (negated, rest_after_prefix) = if let Ok((rest, _)) =
        tag::<_, _, VerboseError<&str>>("it's not a ").parse(lower.as_str())
    {
        (true, Some(rest))
    } else if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("it's a ").parse(lower.as_str()) {
        (false, Some(rest))
    } else if let Ok((rest, _)) =
        tag::<_, _, VerboseError<&str>>("that card is a ").parse(lower.as_str())
    {
        (false, Some(rest))
    } else if let Ok((rest, _)) =
        tag::<_, _, VerboseError<&str>>("it isn't a ").parse(lower.as_str())
    {
        (true, Some(rest))
    } else {
        (false, None)
    };

    if let Some(rest) = rest_after_prefix {
        let rest = rest.trim_end_matches(" card").trim();
        let card_type = match rest {
            "creature" => Some(CoreType::Creature),
            "land" => Some(CoreType::Land),
            "nonland" => {
                return Some(AbilityCondition::RevealedHasCardType {
                    card_type: CoreType::Land,
                    negated: !negated,
                    additional_filter: None,
                });
            }
            "instant" => Some(CoreType::Instant),
            "sorcery" => Some(CoreType::Sorcery),
            "artifact" => Some(CoreType::Artifact),
            "enchantment" => Some(CoreType::Enchantment),
            "planeswalker" => Some(CoreType::Planeswalker),
            "permanent" => None,
            _ => None,
        };
        if let Some(card_type) = card_type {
            return Some(AbilityCondition::RevealedHasCardType {
                card_type,
                negated,
                additional_filter: None,
            });
        }
    }

    let (rest, condition) = parse_inner_condition(&lower).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    static_condition_to_ability_condition(&condition)
}

/// CR 400.7 + CR 608.2c: Parse "a[n] [type] was [verb]'d this way".
///
/// Recognized verbs: `destroyed`, `exiled`, `sacrificed`, `returned`, `discarded`,
/// `milled`, `countered` — the set of imperative verbs that populate a tracked
/// set from their parent effect. Returns the matched type filter plus a
/// negation flag for `wasn't`/`was not`.
///
/// Used by Shredder's Technique ("if an enchantment was destroyed this way")
/// and parallel patterns where a conditional in the same clause tests the type
/// of the single parent target after the preceding effect resolved.
fn parse_a_type_was_verbed_this_way(lower: &str) -> Option<(TypeFilter, bool)> {
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("an "),
        tag::<_, _, VerboseError<&str>>("a "),
    ))
    .parse(lower)
    .ok()?;

    let (rest, type_filter) = alt((
        value(
            TypeFilter::Creature,
            tag::<_, _, VerboseError<&str>>("creature"),
        ),
        value(TypeFilter::Land, tag("land")),
        value(TypeFilter::Artifact, tag("artifact")),
        value(TypeFilter::Enchantment, tag("enchantment")),
        value(TypeFilter::Planeswalker, tag("planeswalker")),
        value(TypeFilter::Instant, tag("instant")),
        value(TypeFilter::Sorcery, tag("sorcery")),
    ))
    .parse(rest)
    .ok()?;

    let (rest, negated) = alt((
        value(true, tag::<_, _, VerboseError<&str>>(" wasn't ")),
        value(true, tag(" was not ")),
        value(false, tag(" was ")),
    ))
    .parse(rest)
    .ok()?;

    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("destroyed"),
        tag("exiled"),
        tag("sacrificed"),
        tag("returned"),
        tag("discarded"),
        tag("milled"),
        tag("countered"),
    ))
    .parse(rest)
    .ok()?;

    let (rest, _) = tag::<_, _, VerboseError<&str>>(" this way")
        .parse(rest)
        .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some((type_filter, negated))
}
