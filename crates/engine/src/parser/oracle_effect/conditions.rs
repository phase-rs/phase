use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::{all_consuming, opt, value};
use nom::sequence::preceded;
use nom::Parser;
use nom_language::error::VerboseError;

use super::super::oracle_nom::bridge::{nom_on_lower, nom_parse_lower};
use super::super::oracle_nom::primitives as nom_primitives;
use super::super::oracle_nom::quantity as nom_quantity;
use super::super::oracle_quantity::{canonicalize_quantity_ref, parse_cda_quantity};
use super::super::oracle_target::parse_type_phrase;
use super::super::oracle_util::{parse_comparison_suffix, TextPair};
use super::counter::normalize_counter_type;
use super::{parse_effect_chain, scan_contains_phrase};
use crate::parser::oracle_warnings::push_warning;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, CastVariantPaid, Comparator, ControllerRef,
    Duration, Effect, FilterProp, ObjectScope, QuantityExpr, QuantityRef, StaticCondition,
    TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::counter::CounterMatch;
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::zones::Zone;

/// Wrap `cond` in `AbilityCondition::Not` when `negated` is true; otherwise
/// return it unchanged. Replaces the per-leaf `negated: bool` fields that
/// existed before Π-N — call sites that previously emitted `Variant { ...,
/// negated }` now construct the positive variant and pass through this helper.
fn maybe_negate(cond: AbilityCondition, negated: bool) -> AbilityCondition {
    if negated {
        AbilityCondition::Not {
            condition: Box::new(cond),
        }
    } else {
        cond
    }
}

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

pub(crate) fn strip_leading_general_conditional(text: &str) -> (Option<AbilityCondition>, String) {
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

/// CR 702.33b + CR 702.33c + CR 702.33f: Recognize quantified or per-variant
/// kicker gating in a leading `"if [subject] was kicked …, [body]"` clause.
/// Returns the typed `AbilityCondition` and the residual body when matched.
///
/// Patterns covered (subject is consumed permissively up to "was kicked"):
/// - "if it was kicked twice, [body]"             → min_count = 2
/// - "if it was kicked three times, [body]"       → min_count = N (English/digit)
/// - "if it was kicked with its {COST} kicker, [body]"
///   → parser records the printed cost; synthesis maps it to the card's
///   positional `KickerVariant` once kicker declarations are visible.
fn strip_quantified_kicker_conditional(
    text: &str,
    lower: &str,
) -> Option<(AbilityCondition, String)> {
    // CR 603.4: Locate the "was kicked" anchor. Subject (~/it/this creature/
    // this spell) is consumed permissively — the typed shape is determined
    // entirely by what follows.
    let after_if = tag::<_, _, VerboseError<&str>>("if ")
        .parse(lower)
        .ok()
        .map(|(rest, _)| rest)?;
    let (after_kicked, _) = take_until::<_, _, VerboseError<&str>>("was kicked")
        .parse(after_if)
        .ok()?;
    let (after_kicked, _) = tag::<_, _, VerboseError<&str>>("was kicked")
        .parse(after_kicked)
        .ok()?;

    // Branch 1: "was kicked with its {COST} kicker, [body]" — per-variant.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>(" with its ").parse(after_kicked) {
        let cost_start = text.len() - rest.len();
        let (rest, cost_text) = take_until::<_, _, VerboseError<&str>>(" kicker, ")
            .parse(rest)
            .ok()?;
        let (rest, _) = tag::<_, _, VerboseError<&str>>(" kicker, ")
            .parse(rest)
            .ok()?;
        let offset = text.len() - rest.len();
        let cost =
            parse_kicker_condition_mana_cost(&text[cost_start..cost_start + cost_text.len()])?;
        return Some((
            AbilityCondition::additional_cost_paid_kicker_cost(cost),
            text[offset..].to_string(),
        ));
    }

    // Branch 2: "was kicked twice, [body]" → min_count = 2.
    // CR 702.33b/c: "twice" is the printed form for kicked-N=2.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>(" twice, ").parse(after_kicked) {
        let offset = text.len() - rest.len();
        return Some((
            AbilityCondition::additional_cost_paid_n_times(2),
            text[offset..].to_string(),
        ));
    }

    // Branch 3: "was kicked N times, [body]" → min_count = N. Accepts both
    // English number words (one through twenty) and digit forms via
    // `nom_primitives::parse_number`. "one time" is unprinted but harmless.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>(" ").parse(after_kicked) {
        if let Ok((rest, n)) = nom_primitives::parse_number(rest) {
            if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>(" times, ").parse(rest) {
                let offset = text.len() - rest.len();
                return Some((
                    AbilityCondition::additional_cost_paid_n_times(n),
                    text[offset..].to_string(),
                ));
            }
        }
    }

    None
}

fn parse_kicker_condition_mana_cost(cost_text: &str) -> Option<ManaCost> {
    nom_primitives::parse_mana_cost
        .parse(cost_text.trim())
        .ok()
        .map(|(_, cost)| cost)
}

pub(super) fn strip_additional_cost_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();

    if let Some((_, rest)) = nom_on_lower(text, &lower, |i| {
        value((), tag("if the gift wasn't promised, ")).parse(i)
    }) {
        return (
            Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::additional_cost_paid_any()),
            }),
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
                Some(AbilityCondition::Not {
                    condition: Box::new(AbilityCondition::additional_cost_paid_any()),
                }),
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
        // CR 702.33b/c + CR 702.33f: Quantified / per-variant kicker gating.
        // Try "kicked twice/N times" and "kicked with its {COST} kicker"
        // BEFORE the plain "was kicked" split so the more specific phrasings
        // take priority. Returns early with the typed condition.
        if let Some((cond, rest)) = strip_quantified_kicker_conditional(text, &lower) {
            return (Some(cond), rest);
        }
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
                    (body, AbilityCondition::additional_cost_paid_any())
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

    // CR 400.7 + CR 608.2c + CR 303.4f + CR 301.5b: "if a[n] [type] (is|was)
    // [verb] this way, [body]" — delegate to the shared
    // `parse_zone_changed_this_way_clause` combinator in `oracle_nom::condition`.
    // The combinator covers past + present tense, single-word imperatives
    // (destroyed/exiled/sacrificed/returned/discarded/milled/countered) AND
    // the multi-word "put onto the battlefield" verb, with subtype filters
    // (Aura/Equipment/...) via `parse_type_phrase`. Replaces the prior
    // hand-rolled past-tense / single-word / top-level-type-only matcher.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("if ").parse(lower.as_str()) {
        if let Ok((after_clause, (filter, _negated))) =
            crate::parser::oracle_nom::condition::parse_zone_changed_this_way_clause(rest)
        {
            // Strip leading punctuation/space between "this way" and the body.
            // Possible separators: ", ", ". ", " ".
            let body_lower = after_clause
                .strip_prefix(", ") // allow-noncombinator: structural separator after parsed clause
                .or_else(|| after_clause.strip_prefix(". ")) // allow-noncombinator: structural separator after parsed clause
                .or_else(|| after_clause.strip_prefix(' ')) // allow-noncombinator: structural separator after parsed clause
                .unwrap_or(after_clause);
            let offset = text.len() - body_lower.len();
            return (
                Some(AbilityCondition::ZoneChangedThisWay { filter }),
                text[offset..].to_string(),
            );
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
                Some(AbilityCondition::Not {
                    condition: Box::new(AbilityCondition::SourceEnteredThisTurn),
                }),
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
            Some(maybe_negate(
                AbilityCondition::RevealedHasCardType {
                    card_type,
                    additional_filter,
                },
                negated,
            )),
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
    Some(maybe_negate(
        AbilityCondition::RevealedHasCardType {
            card_type,
            additional_filter: None,
        },
        negated,
    ))
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
            Some(maybe_negate(AbilityCondition::IsYourTurn, negated)),
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
            qty: QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some(counter_type),
            },
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

    // Leading position: "If its mana value was N or less/greater, [effect]."
    if let Ok((rest, _)) =
        tag::<_, _, VerboseError<&str>>("if its mana value was ").parse(lower.as_str())
    {
        if let Some((condition, body)) = parse_past_mana_value_condition_body(text, rest) {
            return (Some(condition), body);
        }
    }

    // Suffix position: "[effect] if its mana value was N or less/greater."
    if let Some((before, after)) = tp.rsplit_around(" if its mana value was ") {
        if let Some((comparator, threshold)) = parse_mana_value_threshold(after.lower) {
            let condition = AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    FilterProp::Cmc {
                        comparator,
                        value: QuantityExpr::Fixed { value: threshold },
                    },
                ])),
                use_lki: true,
            };
            return (
                Some(condition),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }

    // Suffix position: "[effect] if its mana value is less than or equal to [quantity]."
    if let Some((before, after)) = tp.rsplit_around(" if its mana value is ") {
        if let Some((comparator, value)) = parse_dynamic_mana_value_threshold(after.lower) {
            let condition = AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(
                    TypedFilter::default().properties(vec![FilterProp::Cmc { comparator, value }]),
                ),
                use_lki: false,
            };
            return (
                Some(condition),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }

    // Suffix position: "[effect] if it has mana value N or less/greater."
    if let Some((before, after)) = tp.rsplit_around(" if it has mana value ") {
        if let Some((comparator, threshold)) = parse_mana_value_threshold(after.lower) {
            let prop = match comparator {
                Comparator::LE => FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: threshold },
                },
                Comparator::GE => FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: threshold },
                },
                _ => return (None, text.to_string()),
            };
            let condition = AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![prop])),
                use_lki: false,
            };
            return (
                Some(condition),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }

    (None, text.to_string())
}

fn parse_past_mana_value_condition_body(
    original: &str,
    condition_and_body: &str,
) -> Option<(AbilityCondition, String)> {
    let (rest, threshold) = nom_primitives::parse_number(condition_and_body).ok()?;
    let (rest, _) = tag::<_, _, VerboseError<&str>>(" or ").parse(rest).ok()?;
    let (rest, comparator) = alt((
        value(Comparator::LE, tag::<_, _, VerboseError<&str>>("less")),
        value(Comparator::GE, tag("greater")),
    ))
    .parse(rest)
    .ok()?;
    let rest = rest.trim_start();
    let (rest, _) = tag::<_, _, VerboseError<&str>>(",").parse(rest).ok()?;
    let rest = rest.trim_start();
    let body_start = original.len() - rest.len();
    let condition = AbilityCondition::TargetMatchesFilter {
        filter: TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Cmc {
            comparator,
            value: QuantityExpr::Fixed {
                value: threshold as i32,
            },
        }])),
        use_lki: true,
    };
    Some((condition, original[body_start..].to_string()))
}

fn parse_dynamic_mana_value_threshold(text: &str) -> Option<(Comparator, QuantityExpr)> {
    let text = text.trim().trim_end_matches('.');
    let (rest, comparator) = alt((
        value(
            Comparator::LE,
            tag::<_, _, VerboseError<&str>>("less than or equal to "),
        ),
        value(Comparator::LT, tag("less than ")),
        value(Comparator::GE, tag("greater than or equal to ")),
        value(Comparator::GT, tag("greater than ")),
        value(Comparator::EQ, tag("equal to ")),
    ))
    .parse(text)
    .ok()?;
    let (rest, qty) = nom_quantity::parse_quantity_ref.parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some((
        comparator,
        QuantityExpr::Ref {
            qty: canonicalize_quantity_ref(qty),
        },
    ))
}

pub(super) fn strip_target_supertype_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    if let Ok((rest, _)) =
        tag::<_, _, VerboseError<&str>>("if that land was nonbasic, ").parse(lower.as_str())
    {
        let body_start = text.len() - rest.len();
        return (
            Some(nonbasic_land_lki_condition()),
            text[body_start..].to_string(),
        );
    }

    if let Some((before, after)) = tp.rsplit_around(" if that land was ") {
        if all_consuming(alt((
            tag::<_, _, VerboseError<&str>>("nonbasic."),
            tag("nonbasic"),
        )))
        .parse(after.lower.trim())
        .is_ok()
        {
            return (
                Some(nonbasic_land_lki_condition()),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }

    for (pattern, negated) in &[
        (" if it's ", false),
        (" if it is ", false),
        (" if it isn't ", true),
        (" if it's not ", true),
        (" if it is not ", true),
    ] {
        if let Some((before, after)) = tp.rsplit_around(pattern) {
            let supertype_text = after.lower.trim_end_matches('.').trim();
            let parsed = parse_supertype_word(supertype_text);
            let Ok((rest, supertype)) = parsed else {
                continue;
            };
            if !rest.trim().is_empty() {
                continue;
            }

            let condition = AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(
                    TypedFilter::default()
                        .properties(vec![FilterProp::HasSupertype { value: supertype }]),
                ),
                use_lki: false,
            };
            return (
                Some(maybe_negate(condition, *negated)),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }

    (None, text.to_string())
}

fn nonbasic_land_lki_condition() -> AbilityCondition {
    AbilityCondition::TargetMatchesFilter {
        filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
            FilterProp::NotSupertype {
                value: Supertype::Basic,
            },
        ])),
        use_lki: true,
    }
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

    if let Some(condition) = parse_cast_during_phase_condition_text(text) {
        return Some(condition);
    }

    if let Some(condition) = parse_paid_x_condition_text(text) {
        return Some(condition);
    }

    let (lhs_text, comparator_rhs) = text.split_once(" is ")?;
    let lhs = parse_cda_quantity(lhs_text)?;
    let (comparator, rhs) = parse_quantity_comparison(comparator_rhs)?;
    Some(AbilityCondition::QuantityCheck {
        lhs,
        comparator,
        rhs,
    })
}

fn parse_cast_during_phase_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    nom_parse_lower(&lower, parse_cast_during_phase_condition)
        .map(|phases| AbilityCondition::CastDuringPhase { phases })
}

fn parse_cast_during_phase_condition(
    input: &str,
) -> super::super::oracle_nom::error::OracleResult<'_, Vec<Phase>> {
    all_consuming(|input| {
        let (rest, _) =
            tag::<_, _, VerboseError<&str>>("you cast this spell during your ").parse(input)?;
        alt((
            value(
                vec![Phase::PreCombatMain, Phase::PostCombatMain],
                tag::<_, _, VerboseError<&str>>("main phase"),
            ),
            value(vec![Phase::PreCombatMain], tag("precombat main phase")),
            value(vec![Phase::PostCombatMain], tag("postcombat main phase")),
            value(vec![Phase::Upkeep], tag("upkeep")),
            value(vec![Phase::Draw], tag("draw step")),
            value(vec![Phase::BeginCombat], tag("beginning of combat step")),
            value(vec![Phase::DeclareAttackers], tag("declare attackers step")),
            value(vec![Phase::DeclareBlockers], tag("declare blockers step")),
            value(vec![Phase::CombatDamage], tag("combat damage step")),
            value(vec![Phase::EndCombat], tag("end of combat step")),
            value(vec![Phase::End], tag("end step")),
            value(vec![Phase::Cleanup], tag("cleanup step")),
        ))
        .parse(rest)
    })
    .parse(input)
}

fn parse_paid_x_condition_text(text: &str) -> Option<AbilityCondition> {
    let lower = text.to_ascii_lowercase();
    let (comparator, amount) = nom_parse_lower(&lower, |input| {
        all_consuming(|input| {
            let (rest, _) = tag::<_, _, VerboseError<&str>>("x is ").parse(input)?;
            let (rest, amount) = nom_primitives::parse_number(rest)?;
            let (rest, comparator) = alt((
                value(Comparator::GE, tag::<_, _, VerboseError<&str>>(" or more")),
                value(Comparator::LE, tag(" or less")),
                value(Comparator::GE, tag(" or greater")),
                value(Comparator::LE, tag(" or fewer")),
            ))
            .parse(rest)?;
            let (rest, _) = opt(tag::<_, _, VerboseError<&str>>(".")).parse(rest)?;
            Ok((rest, (comparator, amount)))
        })
        .parse(input)
    })?;

    Some(AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid,
        },
        comparator,
        rhs: QuantityExpr::Fixed {
            value: amount as i32,
        },
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
    result.condition = Some(AbilityCondition::ConditionInstead {
        inner: Box::new(condition),
    });
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

/// CR 122.1 + CR 608.2c: Build an `AbilityCondition` from a counter-threshold
/// `(minimum, maximum)` pair against a counter-quantity expression. Shared by
/// the typed (`CountersOnSelf`) and any-type (`AnyCountersOnSelf`) arms of
/// `static_condition_to_ability_condition` so both round-trip identically.
fn counter_threshold_to_condition(
    qty: QuantityExpr,
    minimum: u32,
    maximum: Option<u32>,
) -> AbilityCondition {
    match (minimum, maximum) {
        // "no counters on ~" — exactly zero.
        (0, Some(0)) => AbilityCondition::QuantityCheck {
            lhs: qty,
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        },
        // "exactly N counters on ~"
        (n, Some(m)) if n == m => AbilityCondition::QuantityCheck {
            lhs: qty,
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
        // "N or fewer counters on ~"
        (0, Some(n)) => AbilityCondition::QuantityCheck {
            lhs: qty,
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
        // "N or more counters on ~" / "a counter on ~" (1+)
        (n, None) => AbilityCondition::QuantityCheck {
            lhs: qty,
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
        // Bounded range "between N and M counters" — express as compound
        // via `And` so each side stays a single QuantityCheck.
        (n, Some(m)) => AbilityCondition::And {
            conditions: vec![
                AbilityCondition::QuantityCheck {
                    lhs: qty.clone(),
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: n as i32 },
                },
                AbilityCondition::QuantityCheck {
                    lhs: qty,
                    comparator: Comparator::LE,
                    rhs: QuantityExpr::Fixed { value: m as i32 },
                },
            ],
        },
    }
}

fn static_condition_to_ability_condition(sc: &StaticCondition) -> Option<AbilityCondition> {
    match sc {
        StaticCondition::DuringYourTurn => Some(AbilityCondition::IsYourTurn),
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
        StaticCondition::IsMonarch => Some(AbilityCondition::IsMonarch),
        StaticCondition::HasCityBlessing => Some(AbilityCondition::HasCityBlessing),
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
            StaticCondition::DuringYourTurn => Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::IsYourTurn),
            }),
            StaticCondition::SourceEnteredThisTurn => Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::SourceEnteredThisTurn),
            }),
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
            StaticCondition::SourceIsTapped => Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::SourceIsTapped),
            }),
            StaticCondition::SourceMatchesFilter { filter } => Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::SourceMatchesFilter {
                    filter: filter.clone(),
                }),
            }),
            _ => None,
        },
        StaticCondition::SourceMatchesFilter { filter } => {
            Some(AbilityCondition::SourceMatchesFilter {
                filter: filter.clone(),
            })
        }
        StaticCondition::SourceIsTapped => Some(AbilityCondition::SourceIsTapped),
        // CR 608.2c: Compound static predicates map recursively to ability
        // conditions. If any child is unmappable, reject the whole compound so
        // the parser does not silently drop part of the condition.
        StaticCondition::And { conditions } => {
            let mapped: Option<Vec<_>> = conditions
                .iter()
                .map(static_condition_to_ability_condition)
                .collect();
            Some(AbilityCondition::And {
                conditions: mapped?,
            })
        }
        StaticCondition::Or { conditions } => {
            let mapped: Option<Vec<_>> = conditions
                .iter()
                .map(static_condition_to_ability_condition)
                .collect();
            Some(AbilityCondition::Or {
                conditions: mapped?,
            })
        }
        // CR 122.1 + CR 608.2c: Counter-threshold gate on the source object.
        // Maps to `QuantityCheck { CountersOn(Self|AnyCountersOnSelf), Comparator, Fixed }`
        // so the existing sub-ability condition evaluator handles it without
        // new runtime support. `CounterMatch::OfType(ct)` reads a single typed
        // counter via `CountersOnSelf`; `CounterMatch::Any` ("no counters on
        // it" / "a counter on it") sums every type via `AnyCountersOnSelf`.
        StaticCondition::HasCounters {
            counters,
            minimum,
            maximum,
        } => {
            let qty = QuantityExpr::Ref {
                qty: match counters {
                    CounterMatch::OfType(ct) => QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: Some(ct.as_str().to_string()),
                    },
                    CounterMatch::Any => QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: None,
                    },
                },
            };
            Some(counter_threshold_to_condition(qty, *minimum, *maximum))
        }
        StaticCondition::DevotionGE { .. }
        | StaticCondition::ChosenColorIs { .. }
        | StaticCondition::SpeedGE { .. }
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
        | StaticCondition::OpponentPoisonAtLeast { .. }
        | StaticCondition::UnlessPay { .. }
        | StaticCondition::Unrecognized { .. }
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

    if let Some(condition) = parse_you_controlled_parent_target_condition(lower.as_str()) {
        return Some(condition);
    }

    if let Some(condition) = parse_zone_change_object_matches_filter_condition(lower.as_str()) {
        return Some(condition);
    }

    if let Some(condition) = parse_target_supertype_condition_text(lower.as_str()) {
        return Some(condition);
    }

    // CR 730.2a: "it's neither day nor night" — Daybound/Nightbound ETB initialization.
    if tag::<_, _, VerboseError<&str>>("it's neither day nor night")
        .parse(lower.as_str())
        .is_ok()
    {
        return Some(AbilityCondition::DayNightIsNeither);
    }

    if tag::<_, _, VerboseError<&str>>("it's the first combat phase of the turn")
        .parse(lower.as_str())
        .is_ok()
    {
        return Some(AbilityCondition::FirstCombatPhaseOfTurn);
    }

    // CR 603.4: "if this is the [Nth] time this ability has resolved this turn"
    // and the abbreviated continuation form "if it's the [Nth] time" used by
    // Omnath's later sentences (the "this ability has resolved this turn" tail
    // is anaphoric to the prior sentence and is dropped). Composes:
    //   subject: "this is" | "it's" | "it is"
    //   ordinal: "first" | "second" | ...
    //   tail:    optional " this ability has resolved this turn"
    if let Some(n) = parse_nth_resolution_condition(lower.as_str()) {
        return Some(AbilityCondition::NthResolutionThisTurn { n });
    }

    if tag::<_, _, VerboseError<&str>>("you win the clash")
        .parse(lower.as_str())
        .is_ok()
        || tag::<_, _, VerboseError<&str>>("you won the clash")
            .parse(lower.as_str())
            .is_ok()
        || tag::<_, _, VerboseError<&str>>("you win")
            .parse(lower.as_str())
            .is_ok()
        || tag::<_, _, VerboseError<&str>>("you won")
            .parse(lower.as_str())
            .is_ok()
    {
        return Some(AbilityCondition::IfYouDo);
    }

    if alt((
        tag::<_, _, VerboseError<&str>>("you don't"),
        tag("you do not"),
        tag("you didn't"),
        tag("you did not"),
    ))
    .parse(lower.as_str())
    .is_ok()
    {
        return Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::IfYouDo),
        });
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
        return Some(maybe_negate(
            AbilityCondition::TargetMatchesFilter {
                filter: TargetFilter::Typed(TypedFilter::new(type_filter)),
                use_lki: true,
            },
            negated,
        ));
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
                return Some(maybe_negate(
                    AbilityCondition::TargetMatchesFilter {
                        filter,
                        use_lki: true,
                    },
                    negated_lki,
                ));
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
                return Some(maybe_negate(
                    AbilityCondition::RevealedHasCardType {
                        card_type: CoreType::Land,
                        additional_filter: None,
                    },
                    !negated,
                ));
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
            return Some(maybe_negate(
                AbilityCondition::RevealedHasCardType {
                    card_type,
                    additional_filter: None,
                },
                negated,
            ));
        }
    }

    let (rest, condition) = parse_inner_condition(&lower).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    static_condition_to_ability_condition(&condition)
}

fn parse_you_controlled_parent_target_condition(lower: &str) -> Option<AbilityCondition> {
    type E<'a> = VerboseError<&'a str>;

    let controller_only =
        TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You));
    let permanent = TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You));
    let nonland_permanent = TargetFilter::Typed(
        TypedFilter::permanent()
            .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
            .controller(ControllerRef::You),
    );
    let card = TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::You));
    let artifact =
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You));
    let creature = TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You));
    let enchantment = TargetFilter::Typed(
        TypedFilter::new(TypeFilter::Enchantment).controller(ControllerRef::You),
    );
    let land = TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You));
    let planeswalker = TargetFilter::Typed(
        TypedFilter::new(TypeFilter::Planeswalker).controller(ControllerRef::You),
    );
    let battle =
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Battle).controller(ControllerRef::You));

    let (_, filter) = all_consuming(preceded(
        tag::<_, _, E>("you controlled "),
        alt((
            value(controller_only, tag::<_, _, E>("it")),
            preceded(
                tag::<_, _, E>("that "),
                alt((
                    value(nonland_permanent, tag::<_, _, E>("nonland permanent")),
                    value(permanent, tag("permanent")),
                    value(artifact, tag("artifact")),
                    value(creature, tag("creature")),
                    value(enchantment, tag("enchantment")),
                    value(land, tag("land")),
                    value(planeswalker, tag("planeswalker")),
                    value(battle, tag("battle")),
                    value(card, tag("card")),
                )),
            ),
        )),
    ))
    .parse(lower)
    .ok()?;

    Some(AbilityCondition::TargetMatchesFilter {
        filter,
        use_lki: true,
    })
}

fn parse_zone_change_object_matches_filter_condition(lower: &str) -> Option<AbilityCondition> {
    let (type_text, negated) = parse_zone_change_object_type_text(lower).ok()?.1;
    let (filter, leftover) = parse_type_phrase(type_text);
    if matches!(filter, TargetFilter::Any) || !leftover.trim().is_empty() {
        return None;
    }

    Some(maybe_negate(
        AbilityCondition::ZoneChangeObjectMatchesFilter {
            origin: None,
            destination: Zone::Battlefield,
            filter,
        },
        negated,
    ))
}

fn parse_zone_change_object_type_text(
    input: &str,
) -> nom::IResult<&str, (&str, bool), VerboseError<&str>> {
    let (input, _) = tag("that ").parse(input)?;
    let (input, _) = alt((
        tag("permanent"),
        tag("enchantment"),
        tag("artifact"),
        tag("creature"),
        tag("equipment"),
        tag("aura"),
        tag("land"),
        tag("token"),
        tag("card"),
    ))
    .parse(input)?;
    let (input, negated) = alt((
        value(
            true,
            alt((
                tag(" is not an "),
                tag(" is not a "),
                tag(" is not "),
                tag(" isn't an "),
                tag(" isn't a "),
                tag(" isn't "),
            )),
        ),
        value(false, alt((tag(" is an "), tag(" is a "), tag(" is ")))),
    ))
    .parse(input)?;
    Ok(("", (input, negated)))
}

fn parse_target_supertype_condition_text(lower: &str) -> Option<AbilityCondition> {
    let (rest, negated) = alt((
        value(
            true,
            alt((
                tag::<_, _, VerboseError<&str>>("it is not "),
                tag("it's not "),
                tag("it isn't "),
            )),
        ),
        value(false, alt((tag("it is "), tag("it's ")))),
    ))
    .parse(lower)
    .ok()?;
    let (rest, supertype) = parse_supertype_word(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    Some(maybe_negate(
        AbilityCondition::TargetMatchesFilter {
            filter: TargetFilter::Typed(
                TypedFilter::default()
                    .properties(vec![FilterProp::HasSupertype { value: supertype }]),
            ),
            use_lki: false,
        },
        negated,
    ))
}

fn parse_supertype_word(input: &str) -> nom::IResult<&str, Supertype, VerboseError<&str>> {
    alt((
        value(
            Supertype::Legendary,
            tag::<_, _, VerboseError<&str>>("legendary"),
        ),
        value(Supertype::Basic, tag("basic")),
        value(Supertype::Snow, tag("snow")),
    ))
    .parse(input)
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
///
/// CR 303.4f / CR 301.5b: Also handles the present-tense "is [verb]ed this
/// way" form and the multi-word "put onto the battlefield" verb so that
/// future LKI-style cards using these tenses (e.g. "if a creature is
/// destroyed this way") parse without code change. The Aura/Equipment ETB
/// continuations (Armored Skyhunter, Vault 101: Birthday Party, Quest for
/// the Holy Relic, Stonehewer Giant) take the dedicated `ZoneChangedThisWay`
/// path in `strip_if_you_do_conditional` because the runtime semantic for
/// "the just-moved card" requires `state.last_zone_changed_ids`, not LKI of
/// the parent target — but extending this function keeps the parser
/// permissive for the LKI-semantic patterns and keeps the two combinators in
/// lockstep on tense + verb coverage.
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

    // CR 400.7 + CR 608.2c: Tense + verb are orthogonal axes. Compose with
    // independent `alt` chains so adding a new verb (or tense) is a single
    // tag arm, not an N×M permutation expansion.
    let (rest, negated) = alt((
        value(true, tag::<_, _, VerboseError<&str>>(" wasn't ")),
        value(true, tag(" isn't ")),
        value(true, tag(" was not ")),
        value(true, tag(" is not ")),
        value(false, tag(" was ")),
        value(false, tag(" is ")),
    ))
    .parse(rest)
    .ok()?;

    let (rest, _) = alt((
        // Multi-word verb listed first: longest-match-wins keeps the
        // single-word `tag("put")` (no such tag here, but defensive against
        // future additions) from short-circuiting the multi-word phrase.
        tag::<_, _, VerboseError<&str>>("put onto the battlefield"),
        tag("destroyed"),
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

/// CR 603.4: Parse "[subject] the [Nth] time[ this ability has resolved this turn]".
///
/// Subject is one of `"this is"`, `"it's"`, `"it is"` — the second/third forms are
/// anaphoric continuations whose "this ability has resolved this turn" tail was
/// printed in a prior sentence. Ordinals span first–tenth (Omnath/Ashling print
/// up to third; the broader ceiling is conservative).
fn parse_nth_resolution_condition(lower: &str) -> Option<u32> {
    type E<'a> = VerboseError<&'a str>;
    let (rest, _) = alt((
        tag::<_, _, E>("this is the "),
        tag("it's the "),
        tag("it is the "),
    ))
    .parse(lower)
    .ok()?;
    let (rest, n) = alt((
        value(1u32, tag::<_, _, E>("first")),
        value(2u32, tag("second")),
        value(3u32, tag("third")),
        value(4u32, tag("fourth")),
        value(5u32, tag("fifth")),
        value(6u32, tag("sixth")),
        value(7u32, tag("seventh")),
        value(8u32, tag("eighth")),
        value(9u32, tag("ninth")),
        value(10u32, tag("tenth")),
    ))
    .parse(rest)
    .ok()?;
    let (rest, _) = tag::<_, _, E>(" time").parse(rest).ok()?;
    let rest = rest.trim_end_matches('.').trim();
    // Tail is optional — anaphoric forms ("if it's the second time") drop it
    // because the prior sentence already established "this ability has resolved
    // this turn" as the subject.
    if rest.is_empty() || rest == "this ability has resolved this turn" {
        Some(n)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_nom::condition::parse_inner_condition;
    use crate::types::counter::{CounterMatch, CounterType};

    #[test]
    fn leading_that_enchantment_is_aura_checks_zone_change_object() {
        let (condition, body) = strip_leading_general_conditional(
            "If that enchantment is an Aura, you may attach it to the token.",
        );
        assert_eq!(body, "you may attach it to the token.");

        let Some(AbilityCondition::ZoneChangeObjectMatchesFilter {
            origin: None,
            destination: Zone::Battlefield,
            filter,
        }) = condition
        else {
            panic!("expected zone-change object filter condition, got {condition:?}");
        };
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed Aura filter");
        };
        assert!(
            filter
                .type_filters
                .iter()
                .any(|ty| matches!(ty, TypeFilter::Subtype(subtype) if subtype == "Aura")),
            "expected Aura subtype filter, got {:?}",
            filter.type_filters
        );
    }

    #[test]
    fn token_then_conditional_aura_attach_targets_created_token() {
        let def = parse_effect_chain(
            "Create a 2/2 white Cat creature token. If that enchantment is an Aura, you may attach it to the token.",
            AbilityKind::Spell,
        );
        let Effect::Token { .. } = *def.effect else {
            panic!("expected token root, got {:?}", def.effect);
        };
        let attach = def
            .sub_ability
            .expect("expected conditional attach sub-ability");
        let Effect::Attach { attachment, target } = &*attach.effect else {
            panic!("expected attach sub-ability, got {:?}", attach.effect);
        };
        assert_eq!(*attachment, TargetFilter::TriggeringSource);
        assert_eq!(*target, TargetFilter::LastCreated);
        assert!(attach.optional);
        assert!(
            matches!(
                attach.condition,
                Some(AbilityCondition::ZoneChangeObjectMatchesFilter {
                    destination: Zone::Battlefield,
                    ..
                })
            ),
            "expected zone-change object condition, got {:?}",
            attach.condition
        );
    }

    #[test]
    fn leading_its_legendary_checks_parent_target_supertype() {
        let (condition, body) =
            strip_leading_general_conditional("If it's legendary, gain 3 life.");
        assert_eq!(body, "gain 3 life.");
        let Some(AbilityCondition::TargetMatchesFilter { filter, use_lki }) = condition else {
            panic!("expected TargetMatchesFilter, got {condition:?}");
        };
        assert!(!use_lki);
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed supertype filter");
        };
        assert!(
            filter.properties.iter().any(|prop| matches!(
                prop,
                FilterProp::HasSupertype {
                    value: Supertype::Legendary
                }
            )),
            "expected Legendary supertype filter, got {:?}",
            filter.properties
        );
    }

    #[test]
    fn leading_this_enchantment_isnt_creature_checks_source_type() {
        let (condition, body) = strip_leading_general_conditional(
            "If this enchantment isn't a creature, it becomes a 3/3 Angel creature with flying.",
        );
        assert_eq!(body, "it becomes a 3/3 Angel creature with flying.");
        assert!(matches!(
            condition,
            Some(AbilityCondition::Not { condition })
                if matches!(*condition, AbilityCondition::SourceMatchesFilter { .. })
        ));
    }

    #[test]
    fn leading_you_win_maps_to_if_you_do_for_clash() {
        let (condition, body) =
            strip_leading_general_conditional("If you win, put a +1/+1 counter on this creature.");
        assert_eq!(body, "put a +1/+1 counter on this creature.");
        assert_eq!(condition, Some(AbilityCondition::IfYouDo));
    }

    #[test]
    fn leading_you_dont_maps_to_not_if_you_do() {
        let (condition, body) = strip_leading_general_conditional(
            "If you didn't put a card into your hand this way, draw a card.",
        );
        assert_eq!(body, "draw a card.");
        assert_eq!(
            condition,
            Some(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::IfYouDo)
            })
        );
    }

    #[test]
    fn dynamic_target_mana_value_suffix_uses_object_count_quantity() {
        let (condition, body) = strip_mana_value_conditional(
            "Put target creature card from an opponent's graveyard onto the battlefield under your control if its mana value is less than or equal to the number of Allies you control.",
        );
        assert_eq!(
            body,
            "Put target creature card from an opponent's graveyard onto the battlefield under your control"
        );
        let Some(AbilityCondition::TargetMatchesFilter { filter, use_lki }) = condition else {
            panic!("expected TargetMatchesFilter, got {condition:?}");
        };
        assert!(!use_lki);
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed filter");
        };
        let [FilterProp::Cmc { comparator, value }] = filter.properties.as_slice() else {
            panic!("expected Cmc property, got {:?}", filter.properties);
        };
        assert_eq!(*comparator, Comparator::LE);
        let QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } = value
        else {
            panic!("expected ObjectCount quantity, got {value:?}");
        };
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed object-count filter");
        };
        assert_eq!(filter.controller, Some(ControllerRef::You));
    }

    /// CR 122.1 + CR 608.2c: "there are no counters on ~" round-trips through
    /// the bridge to a `QuantityCheck` against `AnyCountersOnSelf`. Previously
    /// the bridge returned `None` for `CounterMatch::Any`, which silently
    /// dropped the gate and caused effects (Gemstone Mine, depletion lands)
    /// to fire unconditionally.
    #[test]
    fn bridge_has_counters_any_no_counters_yields_any_counters_on_self_eq_zero() {
        let (rest, sc) = parse_inner_condition("there are no counters on ~").unwrap();
        assert_eq!(rest, "");
        assert_eq!(
            sc,
            StaticCondition::HasCounters {
                counters: CounterMatch::Any,
                minimum: 0,
                maximum: Some(0),
            }
        );
        let bridged = static_condition_to_ability_condition(&sc).expect(
            "CounterMatch::Any must round-trip — None here is the silent-failure regression",
        );
        match bridged {
            AbilityCondition::QuantityCheck {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::CountersOn {
                                scope: ObjectScope::Source,
                                counter_type: None,
                            },
                    },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            } => {}
            other => panic!("unexpected bridged condition: {other:?}"),
        }
    }

    /// `"~ has a counter on it"` (Demon Wall): minimum=1, maximum=None →
    /// `AnyCountersOnSelf >= 1`.
    #[test]
    fn bridge_has_counters_any_at_least_one_yields_any_counters_on_self_ge_one() {
        let (rest, sc) = parse_inner_condition("~ has a counter on it").unwrap();
        assert_eq!(rest, "");
        let bridged = static_condition_to_ability_condition(&sc).expect("must bridge");
        match bridged {
            AbilityCondition::QuantityCheck {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::CountersOn {
                                scope: ObjectScope::Source,
                                counter_type: None,
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            } => {}
            other => panic!("unexpected bridged condition: {other:?}"),
        }
    }

    /// Typed-counter case still routes to `CountersOnSelf { counter_type }` —
    /// confirms the shared `counter_threshold_to_condition` helper preserves the
    /// existing behavior for the OfType branch.
    #[test]
    fn bridge_has_counters_typed_yields_counters_on_self() {
        let sc = StaticCondition::HasCounters {
            counters: CounterMatch::OfType(CounterType::Plus1Plus1),
            minimum: 2,
            maximum: None,
        };
        let bridged = static_condition_to_ability_condition(&sc).expect("must bridge");
        match bridged {
            AbilityCondition::QuantityCheck {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::CountersOn {
                                scope: ObjectScope::Source,
                                counter_type: Some(counter_type),
                            },
                    },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 2 },
            } => assert_eq!(counter_type, "P1P1"),
            other => panic!("unexpected bridged condition: {other:?}"),
        }
    }

    /// CR 702.33d + CR 608.2c: Plain "if it was kicked, …" emits the
    /// default-shape `AdditionalCostPaid` (variant=None, min_count=1) so the
    /// existing single-bool semantics survive. Regression guard for Gift /
    /// Buyback / Bargain / Evidence and Archangel of Wrath's first trigger.
    #[test]
    fn plain_kicked_emits_default_shape() {
        let (cond, body) =
            strip_additional_cost_conditional("If it was kicked, it deals 2 damage to any target.");
        assert_eq!(cond, Some(AbilityCondition::additional_cost_paid_any()));
        assert_eq!(body, "it deals 2 damage to any target.");
    }

    /// CR 702.33b + CR 603.4: "if it was kicked twice, …" → min_count = 2.
    /// Archangel of Wrath's second trigger.
    #[test]
    fn kicked_twice_emits_min_count_two() {
        let (cond, body) = strip_additional_cost_conditional(
            "If it was kicked twice, it deals 2 damage to any target.",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::additional_cost_paid_n_times(2))
        );
        assert_eq!(body, "it deals 2 damage to any target.");
    }

    /// CR 702.33b/c: "if it was kicked three times, …" → min_count = N.
    /// Exercises the `parse_number` English-word path.
    #[test]
    fn kicked_three_times_emits_min_count_three() {
        let (cond, body) =
            strip_additional_cost_conditional("If it was kicked three times, draw a card.");
        assert_eq!(
            cond,
            Some(AbilityCondition::additional_cost_paid_n_times(3))
        );
        assert_eq!(body, "draw a card.");
    }

    /// CR 702.33f: "if it was kicked with its {COST} kicker, …" records the
    /// printed mana cost so synthesis can map it to the positional kicker.
    #[test]
    fn kicked_with_specific_kicker_emits_cost_metadata() {
        let (cond, body) = strip_additional_cost_conditional(
            "If it was kicked with its {2}{U} kicker, target player discards three cards.",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::additional_cost_paid_kicker_cost(
                parse_kicker_condition_mana_cost("{2}{U}").unwrap()
            ))
        );
        assert_eq!(body, "target player discards three cards.");
    }
}
