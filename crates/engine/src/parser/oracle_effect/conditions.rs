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
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, Comparator, ControllerRef, Duration, Effect,
    NinjutsuVariant, QuantityExpr, QuantityRef, StaticCondition, TargetFilter,
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

    for (idx, ch) in text.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => paren_depth += 1,
            ')' if !in_quotes => paren_depth = paren_depth.saturating_sub(1),
            ',' if !in_quotes && paren_depth == 0 => {
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
                Some(AbilityCondition::NinjutsuVariantPaidInstead {
                    variant: NinjutsuVariant::Sneak,
                }),
                after.original.to_string(),
            );
        }
    }
    if body.is_none() && scan_contains_phrase(&lower, "ninjutsu cost was paid") {
        if let Some(after) = tp.strip_after("instead ") {
            return (
                Some(AbilityCondition::NinjutsuVariantPaidInstead {
                    variant: NinjutsuVariant::Ninjutsu,
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
        let remainder = after_type.strip_prefix(", ").unwrap_or(after_type);
        let offset = text.len() - remainder.len();
        return (
            Some(AbilityCondition::RevealedHasCardType { card_type, negated }),
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
    Some(AbilityCondition::RevealedHasCardType { card_type, negated })
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
        is_earthbend: false,
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
            let filter = filter.clone().unwrap_or(TargetFilter::Any);
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
                let filter = filter.clone().unwrap_or(TargetFilter::Any);
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
        | StaticCondition::None => None,
    }
}

pub(super) fn try_nom_condition_as_ability_condition(text: &str) -> Option<AbilityCondition> {
    use crate::parser::oracle_nom::condition::parse_inner_condition;

    let lower = text.to_lowercase();

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
            return Some(AbilityCondition::RevealedHasCardType { card_type, negated });
        }
    }

    let (rest, condition) = parse_inner_condition(&lower).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    static_condition_to_ability_condition(&condition)
}
