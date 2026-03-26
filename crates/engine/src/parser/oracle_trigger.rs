use std::str::FromStr;

use super::oracle_effect::{parse_effect_chain_with_context, ParseContext};
use super::oracle_target::parse_type_phrase;
use super::oracle_util::{
    canonicalize_subtype_name, merge_or_filters, normalize_card_name_refs, parse_number,
    parse_ordinal, parse_subtype, strip_after, strip_reminder_text, TextPair,
};
use crate::types::ability::{
    AbilityKind, ControllerRef, DamageKindFilter, FilterProp, NinjutsuVariant, QuantityExpr,
    QuantityRef, TargetFilter, TriggerCondition, TriggerConstraint, TriggerDefinition, TypeFilter,
    TypedFilter, UnlessCost, UnlessPayModifier,
};
use crate::types::card_type::CoreType;
use crate::types::phase::Phase;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

/// Parse a full trigger line into a TriggerDefinition.
/// Input: a line starting with "When", "Whenever", or "At".
/// The card_name is used for self-reference substitution.
#[tracing::instrument(level = "debug", skip(card_name))]
pub fn parse_trigger_line(text: &str, card_name: &str) -> TriggerDefinition {
    let text = strip_reminder_text(text);
    // Replace self-references: "this creature", "this enchantment", card name → ~
    let normalized = normalize_self_refs(&text, card_name);
    let lower = normalized.to_lowercase();
    let tp = TextPair::new(&normalized, &lower);

    // Split condition from effect at first ", " after the trigger phrase
    let (condition_text, effect_text) = split_trigger(tp);

    let effect_lower = effect_text.to_lowercase();
    // CR 609.3: "You may" at the start of the effect text makes the triggered
    // effect optional at resolution — the player chooses whether to perform it.
    // Mid-chain "you may" is per-sentence optional, handled by
    // parse_effect_chain → strip_optional_effect_prefix().
    let optional = effect_lower.starts_with("you may ");

    // Extract intervening-if condition from effect text
    let (effect_without_if, if_condition) = extract_if_condition(&effect_lower);

    // Strip constraint sentences so they don't leak into effect parsing as sub-abilities
    let effect_final = strip_constraint_sentences(&effect_without_if);

    // CR 118.12: Detect "unless [player] pays {cost}" in effect text.
    // Strip it before effect parsing and capture as UnlessPayModifier.
    let (effect_for_parse, unless_pay) = extract_unless_pay_modifier(&effect_final);

    // CR 608.2k: Extract trigger subject for pronoun resolution in effect text.
    // "it"/"its"/"itself" in the effect refer to the trigger subject, not the source permanent.
    let trigger_subject = extract_trigger_subject_for_context(&condition_text);
    let effect_ctx = ParseContext {
        subject: Some(trigger_subject),
    };

    // Parse the effect
    let has_up_to = effect_for_parse.contains("up to one");
    let execute = if !effect_for_parse.is_empty() {
        let mut ability =
            parse_effect_chain_with_context(&effect_for_parse, AbilityKind::Spell, &effect_ctx);
        if has_up_to {
            ability.optional_targeting = true;
        }
        // CR 609.3: "You may" applies to the effect during resolution, not to whether
        // the trigger fires. Propagate to the execute ability so the resolver prompts
        // the controller via WaitingFor::OptionalEffectChoice.
        if optional {
            ability.optional = true;
        }
        Some(Box::new(ability))
    } else {
        None
    };

    // Parse the condition
    let (_, mut def) = parse_trigger_condition(&condition_text);
    def.execute = execute;
    def.optional = optional;
    def.unless_pay = unless_pay;
    def.condition = if_condition.or(def.condition.take());

    // Check for constraint phrases in the full text.
    // Text-based constraints take precedence; fall back to any constraint already set
    // by the trigger condition parser (e.g. NthSpellThisTurn from try_parse_nth_spell_trigger).
    def.constraint = parse_trigger_constraint(&lower).or(def.constraint.take());

    // Preserve the original oracle text for coverage/UI annotation
    def.description = Some(text.to_string());

    def
}

/// Parse trigger constraint from the full trigger text.
fn parse_trigger_constraint(lower: &str) -> Option<TriggerConstraint> {
    if lower.contains("this ability triggers only once each turn")
        || lower.contains("triggers only once each turn")
        // CR 603.12: "Do this only once each turn" is functionally equivalent.
        || lower.contains("do this only once each turn")
    {
        return Some(TriggerConstraint::OncePerTurn);
    }
    if lower.contains("this ability triggers only once") {
        return Some(TriggerConstraint::OncePerGame);
    }
    if lower.contains("only during your turn") {
        return Some(TriggerConstraint::OnlyDuringYourTurn);
    }
    None
}

/// Strip constraint sentences from effect text so they don't produce spurious sub-abilities.
/// The constraint itself is already extracted by `parse_trigger_constraint` from the full text.
fn strip_constraint_sentences(text: &str) -> String {
    let patterns = [
        "this ability triggers only once each turn.",
        "this ability triggers only once each turn",
        "triggers only once each turn.",
        "triggers only once each turn",
        "this ability triggers only once.",
        "this ability triggers only once",
        "this ability triggers only during your turn.",
        "this ability triggers only during your turn",
        "do this only once each turn.",
        "do this only once each turn",
    ];
    let mut result = text.to_string();
    for pattern in &patterns {
        result = result.replace(pattern, "");
    }
    let result = result.trim().to_string();
    if result.ends_with('.') {
        result[..result.len() - 1].trim().to_string()
    } else {
        result
    }
}

/// CR 118.12: Detect "unless [player] pays {cost}" in trigger effect text.
/// Returns (cleaned effect text without the unless clause, optional UnlessPayModifier).
///
/// Patterns:
/// - "draw a card unless that player pays {X}, where X is ~ power"
/// - "create a token unless that player pays {2}"
fn extract_unless_pay_modifier(text: &str) -> (String, Option<UnlessPayModifier>) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let Some(unless_pos) = tp.find(" unless ") else {
        return (text.to_string(), None);
    };

    let after_unless = &lower[unless_pos + 8..];

    // "unless you [verb]" without "pays" — strip the clause even if we can't
    // fully parse the cost. This ensures the main effect text is clean.
    let Some(pays_pos) = after_unless.find("pays ") else {
        // Still strip the unless clause so the main effect parses correctly.
        let cleaned = text[..unless_pos].trim().to_string();
        return (cleaned, None);
    };
    let cost_str = &after_unless[pays_pos + 5..];

    // Extract cost symbols
    let cost_end = cost_str
        .find(|c: char| c != '{' && c != '}' && !c.is_alphanumeric())
        .unwrap_or(cost_str.len());
    let cost_text = cost_str[..cost_end].trim();

    if cost_text.is_empty() || !cost_text.contains('{') {
        return (text.to_string(), None);
    }

    // Determine the cost type
    let cost = if cost_text == "{x}" || cost_text == "{X}" {
        // Check for "where X is" clause
        let remainder = &cost_str[cost_end..];
        if let Some(quantity) = parse_where_x_is_trigger(remainder) {
            UnlessCost::DynamicGeneric { quantity }
        } else {
            return (text.to_string(), None);
        }
    } else {
        let mana_cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_text);
        if mana_cost == crate::types::mana::ManaCost::NoCost
            || mana_cost == crate::types::mana::ManaCost::zero()
        {
            return (text.to_string(), None);
        }
        UnlessCost::Fixed { cost: mana_cost }
    };

    // Determine payer from text between "unless" and "pays"
    let payer_text = after_unless[..pays_pos].trim();
    let payer = if payer_text.contains("that player") || payer_text.contains("that opponent") {
        TargetFilter::TriggeringPlayer
    } else if payer_text.contains("controller") {
        TargetFilter::TriggeringSpellController
    } else {
        TargetFilter::TriggeringPlayer
    };

    // Strip the unless clause from the effect text
    let cleaned = text[..unless_pos].trim().to_string();

    (cleaned, Some(UnlessPayModifier { cost, payer }))
}

/// Parse "where X is ~'s power" / "where X is this creature's power" etc.
fn parse_where_x_is_trigger(text: &str) -> Option<QuantityExpr> {
    let trimmed = text.trim().trim_start_matches(',').trim();
    let rest = trimmed
        .strip_prefix("where x is ")
        .or_else(|| trimmed.strip_prefix("where X is "))?;
    let rest_lower = rest.to_lowercase();
    if rest_lower.contains("power") {
        Some(QuantityExpr::Ref {
            qty: QuantityRef::SelfPower,
        })
    } else if rest_lower.contains("toughness") {
        Some(QuantityExpr::Ref {
            qty: QuantityRef::SelfToughness,
        })
    } else {
        None
    }
}

/// Extract an intervening-if condition from effect text.
/// Returns (cleaned effect text, optional condition).
///
/// Supports composable predicates: single conditions and compound "X and Y" forms.
/// Each predicate is parsed independently, then composed with `And`/`Or` if needed.
fn extract_if_condition(text: &str) -> (String, Option<TriggerCondition>) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Compound: "if you gained and lost life this turn"
    if let Some(pos) = tp.find("if you gained and lost life this turn") {
        let condition = TriggerCondition::And {
            conditions: vec![
                TriggerCondition::GainedLife { minimum: 1 },
                TriggerCondition::LostLife,
            ],
        };
        return (
            strip_condition_clause(text, pos, "if you gained and lost life this turn".len()),
            Some(condition),
        );
    }

    // "if you gained N or more life this turn" / "if you gained life this turn"
    let gained_patterns = [
        "if you've gained ",
        "if you gained ",
        "if you've gained life this turn",
        "if you gained life this turn",
    ];
    for pattern in &gained_patterns {
        if let Some(pos) = tp.find(pattern) {
            let after = &lower[pos + pattern.len()..];

            if pattern.ends_with("life this turn") {
                return (
                    strip_condition_clause(text, pos, pattern.len()),
                    Some(TriggerCondition::GainedLife { minimum: 1 }),
                );
            }

            if let Some((minimum, tail_len)) = parse_life_threshold(after) {
                let clause_len = pattern.len() + tail_len;
                return (
                    strip_condition_clause(text, pos, clause_len),
                    Some(TriggerCondition::GainedLife { minimum }),
                );
            }

            if after.starts_with("life this turn") {
                let clause_len = pattern.len() + "life this turn".len();
                return (
                    strip_condition_clause(text, pos, clause_len),
                    Some(TriggerCondition::GainedLife { minimum: 1 }),
                );
            }
        }
    }

    // "if you descended this turn"
    if let Some(pos) = tp.find("if you descended this turn") {
        return (
            strip_condition_clause(text, pos, "if you descended this turn".len()),
            Some(TriggerCondition::Descended),
        );
    }

    // "if you cast it" — zoneless cast check (CR 701.57a: Discover ETBs)
    if let Some(pos) = tp.find("if you cast it") {
        // Guard: must not be followed by " from" (which is the zone-specific variant)
        let after = &lower[pos + "if you cast it".len()..];
        if !after.starts_with(" from") {
            return (
                strip_condition_clause(text, pos, "if you cast it".len()),
                Some(TriggerCondition::WasCast),
            );
        }
    }

    // "if you control N or more creatures"
    if let Some((condition, end_pos)) = parse_control_count_condition(&lower) {
        let start = tp.find("if you control ").unwrap();
        return (
            strip_condition_clause(text, start, end_pos - start),
            Some(condition),
        );
    }

    // CR 508.1 / CR 603.4: "if it's attacking" / "if it is attacking"
    for pattern in &["if it's attacking", "if it is attacking"] {
        if let Some(pos) = tp.find(pattern) {
            return (
                strip_condition_clause(text, pos, pattern.len()),
                Some(TriggerCondition::SourceIsAttacking),
            );
        }
    }

    // CR 702.49 + CR 603.4: "if his/her/its sneak cost was paid [this turn]"
    // Guard: "instead" after the condition means this is a conditional override effect
    // (handled by strip_additional_cost_conditional in parse_effect_chain), not an intervening-if.
    if lower.contains("sneak cost was paid") && !lower.contains("instead") {
        let pos = tp.find("if ").unwrap_or(0);
        let end = tp
            .find("sneak cost was paid")
            .map(|p| {
                let after = &lower[p + "sneak cost was paid".len()..];
                let extra = if after.starts_with(" this turn") {
                    " this turn".len()
                } else {
                    0
                };
                p + "sneak cost was paid".len() + extra
            })
            .unwrap_or(lower.len());
        return (
            strip_condition_clause(text, pos, end - pos),
            Some(TriggerCondition::NinjutsuVariantPaid {
                variant: NinjutsuVariant::Sneak,
            }),
        );
    }

    // CR 702.49 + CR 603.4: "if its ninjutsu cost was paid [this turn]"
    if lower.contains("ninjutsu cost was paid") && !lower.contains("instead") {
        let pos = tp.find("if ").unwrap_or(0);
        let end = tp
            .find("ninjutsu cost was paid")
            .map(|p| {
                let after = &lower[p + "ninjutsu cost was paid".len()..];
                let extra = if after.starts_with(" this turn") {
                    " this turn".len()
                } else {
                    0
                };
                p + "ninjutsu cost was paid".len() + extra
            })
            .unwrap_or(lower.len());
        return (
            strip_condition_clause(text, pos, end - pos),
            Some(TriggerCondition::NinjutsuVariantPaid {
                variant: NinjutsuVariant::Ninjutsu,
            }),
        );
    }

    // CR 603.4: "if no spells were cast last turn" — werewolf transform
    if let Some(pos) = tp.find("if no spells were cast last turn") {
        return (
            strip_condition_clause(text, pos, "if no spells were cast last turn".len()),
            Some(TriggerCondition::NoSpellsCastLastTurn),
        );
    }

    // CR 603.4: "if two or more spells were cast last turn" — werewolf reverse
    if let Some(pos) = tp.find("if two or more spells were cast last turn") {
        return (
            strip_condition_clause(text, pos, "if two or more spells were cast last turn".len()),
            Some(TriggerCondition::TwoOrMoreSpellsCastLastTurn),
        );
    }

    // CR 603.4: "if it's not your turn" / "if it isn't your turn"
    for pattern in &["if it's not your turn", "if it isn't your turn"] {
        if let Some(pos) = tp.find(pattern) {
            return (
                strip_condition_clause(text, pos, pattern.len()),
                Some(TriggerCondition::NotYourTurn),
            );
        }
    }

    // "if you control a/an [type]" — general control presence
    for prefix in &["if you control a ", "if you control an "] {
        if let Some(pos) = tp.find(prefix) {
            let after = &text[pos + prefix.len()..];
            let (filter, rest) = crate::parser::oracle_target::parse_type_phrase(after);
            if !matches!(filter, TargetFilter::Any) {
                let consumed = after.len() - rest.len();
                return (
                    strip_condition_clause(text, pos, prefix.len() + consumed),
                    Some(TriggerCondition::ControlsType { filter }),
                );
            }
        }
    }

    // CR 603.4: "if you have N or more life" — life-total threshold condition.
    if let Some(pos) = tp.find("if you have ") {
        let after = &lower[pos + "if you have ".len()..];
        if let Some(or_more_pos) = after.find(" or more life") {
            let life_text = &after[..or_more_pos];
            if let Some((n, remainder)) = parse_number(life_text) {
                if remainder.trim().is_empty() {
                    let clause_len = "if you have ".len() + or_more_pos + " or more life".len();
                    return (
                        strip_condition_clause(text, pos, clause_len),
                        Some(TriggerCondition::LifeTotalGE { minimum: n as i32 }),
                    );
                }
            }
        }
    }

    // CR 400.7 + CR 603.10: "if it was a [type]" / "if it was an [type]"
    for prefix in &["if it was a ", "if it was an "] {
        if let Some(pos) = tp.find(prefix) {
            let after = &lower[pos + prefix.len()..];
            let type_word = after.split_whitespace().next().unwrap_or("");
            let trimmed = type_word.trim_end_matches(',').trim_end_matches('.');
            let capitalized = format!("{}{}", &trimmed[..1].to_uppercase(), &trimmed[1..]);
            if let Ok(card_type) = CoreType::from_str(&capitalized) {
                let clause_len = prefix.len() + trimmed.len();
                return (
                    strip_condition_clause(text, pos, clause_len),
                    Some(TriggerCondition::WasType { card_type }),
                );
            }
        }
    }

    (text.to_string(), None)
}

/// Strip a condition clause from text, joining the before and after portions.
/// Handles the clause appearing at the start, end, or middle of the text.
fn strip_condition_clause(text: &str, clause_start: usize, clause_len: usize) -> String {
    let before = text[..clause_start].trim_end().trim_end_matches(',');
    let after = text[clause_start + clause_len..]
        .trim_start_matches(',')
        .trim_start()
        .trim_end_matches('.')
        .trim();
    if before.is_empty() {
        after.to_string()
    } else if after.is_empty() {
        before.to_string()
    } else {
        format!("{before} {after}")
    }
}

/// Parse "if you control N or more creatures" → (condition, end_byte_offset)
fn parse_control_count_condition(lower: &str) -> Option<(TriggerCondition, usize)> {
    let start = lower.find("if you control ")?;
    let after_prefix = &lower[start + "if you control ".len()..];
    let (n, rest) = parse_number(after_prefix)?;
    if rest.starts_with("or more creatures") {
        let end = start
            + "if you control ".len()
            + (after_prefix.len() - rest.len())
            + "or more creatures".len();
        return Some((TriggerCondition::ControlCreatures { minimum: n }, end));
    }
    None
}

/// Parse "N or more life this turn" → N, or "life this turn" → 1
/// Parse "N or more life this turn" → (minimum, bytes_consumed).
/// `bytes_consumed` is measured from the start of `text` (including leading whitespace)
/// so the caller can compute the exact clause length for stripping.
fn parse_life_threshold(text: &str) -> Option<(u32, usize)> {
    let leading = text.len() - text.trim_start().len();
    let trimmed = text.trim_start();
    // "3 or more life this turn"
    let space = trimmed.find(' ')?;
    let n = trimmed[..space].parse::<u32>().ok()?;
    let after_num = &trimmed[space..];
    // Match the full tail: " or more life this turn"
    let tail = " or more life this turn";
    if after_num.starts_with(tail) {
        Some((n, leading + space + tail.len()))
    } else {
        // Fallback: just the number was recognizable but no standard tail
        None
    }
}

fn normalize_self_refs(text: &str, card_name: &str) -> String {
    normalize_card_name_refs(text, card_name)
}

fn split_trigger(tp: TextPair<'_>) -> (String, String) {
    if let Some(comma_pos) = find_effect_boundary(tp.lower) {
        let condition = tp.original[..comma_pos].trim().to_string();
        let effect = tp.original[comma_pos + 2..].trim().to_string();
        (condition, effect)
    } else {
        (tp.original.to_string(), String::new())
    }
}

fn find_effect_boundary(lower: &str) -> Option<usize> {
    lower.find(", ")
}

fn make_base() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Unknown("unknown".to_string()))
        .trigger_zones(vec![Zone::Battlefield])
}

pub(crate) fn parse_trigger_condition(condition: &str) -> (TriggerMode, TriggerDefinition) {
    let lower = condition.to_lowercase();

    if let Some(result) = try_parse_named_trigger_mode(&lower) {
        return result;
    }

    if let Some(result) = try_parse_special_trigger_pattern(&lower) {
        return result;
    }

    // --- Phase triggers: "At the beginning of..." ---
    if let Some(result) = try_parse_phase_trigger(&lower) {
        return result;
    }

    // --- Player triggers: "you gain life", "you cast a spell", "you draw a card" ---
    if let Some(result) = try_parse_player_trigger(&lower) {
        return result;
    }

    // --- Subject + event decomposition ---
    // Strip leading "when"/"whenever"
    let after_keyword = lower
        .strip_prefix("whenever ")
        .or_else(|| lower.strip_prefix("when "))
        .unwrap_or(&lower);

    // Parse the subject ("~", "another creature you control", "a creature", etc.)
    // CR 603.2c: Detect "one or more" quantifier for batched trigger semantics
    let is_batched = after_keyword.starts_with("one or more ");
    let (subject, rest) = parse_trigger_subject(after_keyword);

    // Parse event verb from the remaining text
    if let Some((mode, mut def)) = try_parse_event(&subject, rest, &lower) {
        if is_batched {
            def.batched = true;
        }
        return (mode, def);
    }

    // --- Fallback ---
    let mut def = make_base();
    let mode = TriggerMode::Unknown(condition.to_string());
    def.mode = mode.clone();
    def.description = Some(condition.to_string());
    (mode, def)
}

/// CR 608.2k: Extract the trigger subject from condition text for pronoun context.
/// Reuses `parse_trigger_subject` but only needs the `TargetFilter`, not the remainder.
/// For phase triggers ("at the beginning of..."), the subject is unrecognized and
/// `resolve_it_pronoun` will fall back to `SelfRef`.
fn extract_trigger_subject_for_context(condition_text: &str) -> TargetFilter {
    let lower = condition_text.to_lowercase();
    let after_keyword = lower
        .strip_prefix("whenever ")
        .or_else(|| lower.strip_prefix("when "))
        .unwrap_or(&lower);
    let (subject, _) = parse_trigger_subject(after_keyword);
    subject
}

// ---------------------------------------------------------------------------
// Subject parsing: extracts the trigger subject filter and remaining text
// ---------------------------------------------------------------------------

/// Parse a trigger subject from the beginning of the condition text (after when/whenever).
/// Returns (TargetFilter for valid_card, remaining text after subject).
///
/// Handles compound subjects joined by "or":
///   "~ or another creature or artifact you control enters"
///   → Or { SelfRef, Typed{Creature, You, [Another]}, Typed{Artifact, You, [Another]} }
///   with remaining text "enters"
fn parse_trigger_subject(text: &str) -> (TargetFilter, &str) {
    let (first, rest) = parse_single_subject(text);

    // Check for "or " combinator to build compound subjects
    let rest_trimmed = rest.trim_start();
    if let Some(after_or) = rest_trimmed.strip_prefix("or ") {
        let (second, final_rest) = parse_trigger_subject(after_or);
        return (merge_or_filters(first, second), final_rest);
    }

    (first, rest)
}

/// Parse a single (non-compound) trigger subject.
fn parse_single_subject(text: &str) -> (TargetFilter, &str) {
    // Self-reference: "~"
    if let Some(rest) = text.strip_prefix("~ ") {
        return (TargetFilter::SelfRef, rest);
    }
    if text == "~" {
        return (TargetFilter::SelfRef, "");
    }

    if let Some(rest) = text.strip_prefix("this ") {
        let noun_end = rest.find(' ').unwrap_or(rest.len());
        if noun_end > 0 {
            return (TargetFilter::SelfRef, rest[noun_end..].trim_start());
        }
    }

    // "equipped creature" / "enchanted creature" — the permanent this card is attached to
    if let Some(rest) = text.strip_prefix("equipped creature ") {
        return (TargetFilter::AttachedTo, rest);
    }
    if text == "equipped creature" {
        return (TargetFilter::AttachedTo, "");
    }
    if let Some(rest) = text.strip_prefix("enchanted creature ") {
        return (TargetFilter::AttachedTo, rest);
    }
    if text == "enchanted creature" {
        return (TargetFilter::AttachedTo, "");
    }
    if let Some(rest) = text.strip_prefix("enchanted land ") {
        return (TargetFilter::AttachedTo, rest);
    }
    if text == "enchanted land" {
        return (TargetFilter::AttachedTo, "");
    }
    // "enchanted permanent" (some aura triggers use this phrasing)
    if let Some(rest) = text.strip_prefix("enchanted permanent ") {
        return (TargetFilter::AttachedTo, rest);
    }
    if text == "enchanted permanent" {
        return (TargetFilter::AttachedTo, "");
    }

    // "another <type phrase>" — compose with FilterProp::Another
    if let Some(after_another) = text.strip_prefix("another ") {
        let (filter, rest) = parse_type_phrase(after_another);
        let with_another = add_another_prop(filter);
        return (with_another, rest);
    }

    if let Some(after_quantifier) = text.strip_prefix("one or more ") {
        let (filter, rest) = parse_type_phrase(after_quantifier);
        if rest.len() < after_quantifier.len() {
            return (filter, rest);
        }
    }

    // "a "/"an " + type phrase (general subject)
    let after_article = text.strip_prefix("a ").or_else(|| text.strip_prefix("an "));
    if let Some(after) = after_article {
        let (filter, rest) = parse_type_phrase(after);
        return (filter, rest);
    }

    let (filter, rest) = parse_type_phrase(text);
    if rest.len() < text.len() {
        return (filter, rest);
    }

    // Fallback: no subject parsed, return Any
    (TargetFilter::Any, text)
}

/// Add FilterProp::Another to a TargetFilter. Distributes into Or branches recursively.
fn add_another_prop(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            mut properties,
        }) => {
            properties.push(FilterProp::Another);
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller,
                properties,
            })
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(add_another_prop).collect(),
        },
        _ => TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Another])),
    }
}

fn add_controller(filter: TargetFilter, controller: ControllerRef) -> TargetFilter {
    match filter {
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: existing,
            properties,
        }) => TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: Some(existing.unwrap_or(controller)),
            properties,
        }),
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|filter| add_controller(filter, controller.clone()))
                .collect(),
        },
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Event verb parsing: matches the event after the subject
// ---------------------------------------------------------------------------

/// Try to parse an event verb and build a TriggerDefinition from subject + event.
fn try_parse_event(
    subject: &TargetFilter,
    rest: &str,
    full_lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    let rest = rest.trim_start();

    // "enters or attacks" / "enters the battlefield or attacks" — compound trigger
    if rest.starts_with("enters or attacks")
        || rest.starts_with("enters the battlefield or attacks")
    {
        let mut def = make_base();
        def.mode = TriggerMode::EntersOrAttacks;
        def.destination = Some(Zone::Battlefield);
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::EntersOrAttacks, def));
    }

    // "attacks or blocks" — compound trigger
    if rest.starts_with("attacks or blocks") {
        let mut def = make_base();
        def.mode = TriggerMode::AttacksOrBlocks;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::AttacksOrBlocks, def));
    }

    // "enters [the battlefield]" / "enter [the battlefield]" (plural for "one or more" subjects)
    // Also handles "enters from your hand" (origin filter).
    if rest.starts_with("enter") {
        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.destination = Some(Zone::Battlefield);
        def.valid_card = Some(subject.clone());

        // CR 702.49c: "enters from your hand" — set origin zone.
        let rest_lower = rest.to_lowercase();
        if rest_lower.contains("from your hand") {
            def.origin = Some(Zone::Hand);
        }

        return Some((TriggerMode::ChangesZone, def));
    }

    // CR 700.4: "Dies"/"die" means "is put into a graveyard from the battlefield."
    if rest.starts_with("die")
        || rest.starts_with("is put into a graveyard from the battlefield")
        || rest.starts_with("are put into a graveyard from the battlefield")
    {
        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.origin = Some(Zone::Battlefield);
        def.destination = Some(Zone::Graveyard);
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::ChangesZone, def));
    }

    // CR 120.1: "deals combat damage" / "deal combat damage" (plural for &-names after ~ normalization)
    if rest.starts_with("deals combat damage") || rest.starts_with("deal combat damage") {
        let mut def = make_base();
        def.mode = TriggerMode::DamageDone;
        def.damage_kind = DamageKindFilter::CombatOnly;
        def.valid_source = Some(subject.clone());
        return Some((TriggerMode::DamageDone, def));
    }

    // CR 120.1: "deals damage" / "deal damage" (plural for &-names)
    if rest.starts_with("deals damage") || rest.starts_with("deal damage") {
        let mut def = make_base();
        def.mode = TriggerMode::DamageDone;
        def.valid_source = Some(subject.clone());
        return Some((TriggerMode::DamageDone, def));
    }

    // CR 508.1a: "~ and at least N other creatures attack" (Battalion/Pack Tactics)
    if let Some(after_and) = rest
        .strip_prefix("and at least ")
        .or_else(|| rest.strip_prefix("and "))
    {
        if after_and.contains("attack") {
            // Parse N from "two other creatures attack" / "one other creature attacks"
            if let Some((n, _rest_after_n)) = parse_number(after_and) {
                let mut def = make_base();
                def.mode = TriggerMode::Attacks;
                def.valid_card = Some(subject.clone());
                def.condition = Some(TriggerCondition::MinCoAttackers { minimum: n });
                return Some((TriggerMode::Attacks, def));
            }
        }
    }

    // "attacks" (singular) or "attack" (plural — multi-name cards like "Raph & Leo")
    // Guard against false-matching "attacker"/"attacking".
    if rest.starts_with("attacks")
        || (rest.starts_with("attack")
            && !rest.starts_with("attacker")
            && !rest.starts_with("attacking"))
    {
        let mut def = make_base();
        def.mode = TriggerMode::Attacks;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Attacks, def));
    }

    // "blocks" — fires for the blocking creature.
    // "blocks or becomes blocked" is parsed as Blocks only (blocker side).
    if rest.starts_with("blocks") {
        let mut def = make_base();
        def.mode = TriggerMode::Blocks;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Blocks, def));
    }

    // "leaves the battlefield"
    if rest.starts_with("leaves the battlefield") || rest.starts_with("leaves") {
        let mut def = make_base();
        def.mode = TriggerMode::LeavesBattlefield;
        def.valid_card = Some(subject.clone());
        // LTB triggers fire from the graveyard (object has already moved)
        def.trigger_zones = vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile];
        return Some((TriggerMode::LeavesBattlefield, def));
    }

    // CR 700.4: "is put into a graveyard from [zone]" / "is put into [possessive] graveyard [from zone]"
    // Generalized handler for all "put into graveyard" zone-change patterns.
    // Covers: "is/are put into a/your/an opponent's graveyard [from the battlefield/anywhere/your library]"
    if let Some(result) = try_parse_put_into_graveyard(subject, rest) {
        return Some(result);
    }

    // "becomes blocked"
    if rest.starts_with("becomes blocked") {
        let mut def = make_base();
        def.mode = TriggerMode::BecomesBlocked;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::BecomesBlocked, def));
    }

    if rest.starts_with("becomes the target of a spell or ability") {
        let mut def = make_base();
        def.mode = TriggerMode::BecomesTarget;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::BecomesTarget, def));
    }

    // CR 114.1a: "becomes the target of a spell" (spell only, no abilities)
    if rest.strip_prefix("becomes the target of a spell").is_some() {
        let mut def = make_base();
        def.mode = TriggerMode::BecomesTarget;
        def.valid_card = Some(subject.clone());
        def.valid_source = Some(TargetFilter::StackSpell);
        return Some((TriggerMode::BecomesTarget, def));
    }

    // "is dealt combat damage" / "is dealt damage"
    if rest.starts_with("is dealt combat damage") {
        let mut def = make_base();
        def.mode = TriggerMode::DamageReceived;
        def.damage_kind = DamageKindFilter::CombatOnly;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::DamageReceived, def));
    }
    if rest.starts_with("is dealt damage") {
        let mut def = make_base();
        def.mode = TriggerMode::DamageReceived;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::DamageReceived, def));
    }

    // "becomes tapped"
    if rest.starts_with("becomes tapped") {
        let mut def = make_base();
        def.mode = TriggerMode::Taps;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Taps, def));
    }

    if rest.starts_with("is tapped for mana") {
        let mut def = make_base();
        def.mode = TriggerMode::TapsForMana;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::TapsForMana, def));
    }

    if rest.starts_with("becomes untapped") || rest.starts_with("untaps") {
        let mut def = make_base();
        def.mode = TriggerMode::Untaps;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Untaps, def));
    }

    if rest.starts_with("is turned face up") {
        let mut def = make_base();
        def.mode = TriggerMode::TurnFaceUp;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::TurnFaceUp, def));
    }

    if rest.starts_with("mutates") {
        let mut def = make_base();
        def.mode = TriggerMode::Mutates;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Mutates, def));
    }

    // CR 702.110c: "exploits a creature" — exploit trigger
    if rest.starts_with("exploits a creature") || rest.starts_with("exploits") {
        let mut def = make_base();
        def.mode = TriggerMode::Exploited;
        def.valid_card = Some(subject.clone());
        return Some((TriggerMode::Exploited, def));
    }

    // CR 712.14: "transforms" / "transforms into" — transform trigger
    if rest.starts_with("transforms") {
        let mut def = make_base();
        def.mode = TriggerMode::Transformed;
        def.valid_source = Some(subject.clone());
        return Some((TriggerMode::Transformed, def));
    }

    // Counter-related events: "a +1/+1 counter is put on ~" / "one or more counters are put on ~"
    if let Some(result) = try_parse_counter_trigger(full_lower) {
        return Some(result);
    }

    None
}

fn try_parse_named_trigger_mode(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let mut def = make_base();

    if matches!(lower, "whenever chaos ensues" | "when chaos ensues") {
        def.mode = TriggerMode::ChaosEnsues;
        return Some((TriggerMode::ChaosEnsues, def));
    }

    if matches!(
        lower,
        "when you set this scheme in motion" | "whenever you set this scheme in motion"
    ) {
        def.mode = TriggerMode::SetInMotion;
        return Some((TriggerMode::SetInMotion, def));
    }

    if matches!(
        lower,
        "whenever you crank this contraption"
            | "when you crank this contraption"
            | "whenever you crank this ~"
            | "when you crank this ~"
    ) {
        def.mode = TriggerMode::CrankContraption;
        return Some((TriggerMode::CrankContraption, def));
    }

    None
}

fn try_parse_special_trigger_pattern(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    if let Some(result) = try_parse_self_or_another_controlled_subtype_enters(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_another_controlled_subtype_enters(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_controlled_subtype_attacks(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_one_or_more_combat_damage_to_player(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_n_or_more_attacks(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_one_or_more_die(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_one_or_more_leave_graveyard(lower) {
        return Some(result);
    }

    if let Some(result) = try_parse_one_or_more_put_into_graveyard(lower) {
        return Some(result);
    }

    // CR 120.1b: "a source you control deals noncombat damage to an opponent"
    for prefix in [
        "whenever a source you control deals noncombat damage to an opponent",
        "when a source you control deals noncombat damage to an opponent",
    ] {
        if lower == prefix {
            let mut def = make_base();
            def.mode = TriggerMode::DamageDone;
            def.damage_kind = DamageKindFilter::NoncombatOnly;
            def.valid_source = Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ));
            def.valid_target = Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ));
            return Some((TriggerMode::DamageDone, def));
        }
    }

    if matches!(
        lower,
        "whenever you commit a crime" | "when you commit a crime"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::CommitCrime;
        return Some((TriggerMode::CommitCrime, def));
    }

    if matches!(
        lower,
        "whenever day becomes night or night becomes day"
            | "when day becomes night or night becomes day"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::DayTimeChanges;
        return Some((TriggerMode::DayTimeChanges, def));
    }

    if matches!(
        lower,
        "when you unlock this door" | "whenever you unlock this door"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::UnlockDoor;
        return Some((TriggerMode::UnlockDoor, def));
    }

    // CR 508.1a: "enchanted player is attacked" — the aura enchants a player,
    // and the trigger fires when any creature attacks that player.
    for prefix in [
        "whenever enchanted player is attacked",
        "when enchanted player is attacked",
    ] {
        if lower.starts_with(prefix) {
            let mut def = make_base();
            def.mode = TriggerMode::Attacks;
            // AttachedTo here references the player the aura is attached to
            def.valid_target = Some(TargetFilter::AttachedTo);
            return Some((TriggerMode::Attacks, def));
        }
    }

    for prefix in ["whenever you cast or copy ", "when you cast or copy "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            if matches!(
                rest,
                "an instant or sorcery spell" | "a instant or sorcery spell"
            ) {
                let mut def = make_base();
                def.mode = TriggerMode::SpellCastOrCopy;
                def.valid_card = Some(TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Instant)),
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
                    ],
                });
                def.valid_target = Some(TargetFilter::Controller);
                return Some((TriggerMode::SpellCastOrCopy, def));
            }
        }
    }

    // CR 700.4 + CR 120.1: "a creature dealt damage by ~ this turn dies"
    // This is a death trigger gated on the dying creature having received damage from
    // the trigger source during the current turn. Maps to ChangesZone (dies) with
    // a DealtDamageBySourceThisTurn condition.
    for prefix in [
        "whenever a creature dealt damage by ~ this turn dies",
        "when a creature dealt damage by ~ this turn dies",
    ] {
        if lower.starts_with(prefix) {
            let mut def = make_base();
            def.mode = TriggerMode::ChangesZone;
            def.origin = Some(Zone::Battlefield);
            def.destination = Some(Zone::Graveyard);
            def.valid_card = Some(TargetFilter::Typed(TypedFilter::creature()));
            def.condition = Some(TriggerCondition::DealtDamageBySourceThisTurn);
            return Some((TriggerMode::ChangesZone, def));
        }
    }

    None
}

/// Parse "whenever N or more creatures [you control] attack [a player]" patterns.
/// CR 508.1a: Handles both "one or more" and "two or more" quantifiers.
fn try_parse_n_or_more_attacks(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    for (prefix, min_count) in [
        ("whenever one or more ", 1u32),
        ("when one or more ", 1),
        ("whenever two or more ", 2),
        ("when two or more ", 2),
    ] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        // Strip optional " a player" target suffix before checking for "attack"
        let (subject_text, attacks_player) =
            if let Some(before) = rest.strip_suffix(" attack a player") {
                (before, true)
            } else if let Some(before) = rest.strip_suffix(" attack") {
                (before, false)
            } else if let Some(before) = rest.strip_suffix(" attacks") {
                (before, false)
            } else {
                continue;
            };

        let (filter, remainder) = parse_type_phrase(subject_text);
        if !remainder.trim().is_empty() {
            continue;
        }

        let mut def = make_base();
        def.mode = TriggerMode::YouAttack;
        def.valid_card = Some(filter);
        if attacks_player {
            def.valid_target = Some(TargetFilter::Player);
        }
        if min_count > 1 {
            def.condition = Some(TriggerCondition::MinCoAttackers {
                minimum: min_count - 1,
            });
        }
        return Some((TriggerMode::YouAttack, def));
    }

    None
}

/// Parse "whenever one or more [subject] die" patterns.
/// CR 603.10c: "One or more" triggers fire once per batch of simultaneous events.
fn try_parse_one_or_more_die(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever one or more ", "when one or more "] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        let Some(subject_text) = rest
            .strip_suffix(" die")
            .or_else(|| rest.strip_suffix(" dies"))
        else {
            continue;
        };

        let (filter, remainder) = parse_type_phrase(subject_text);
        if !remainder.trim().is_empty() {
            continue;
        }

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZoneAll;
        def.origin = Some(Zone::Battlefield);
        def.destination = Some(Zone::Graveyard);
        def.valid_card = Some(filter);
        def.batched = true;
        return Some((TriggerMode::ChangesZoneAll, def));
    }

    None
}

/// Parse "whenever one or more [subject] cards leave your graveyard" patterns.
/// CR 603.10c: "One or more" triggers fire once per batch of simultaneous events.
fn try_parse_one_or_more_leave_graveyard(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever one or more ", "when one or more "] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };

        // Strip trailing constraint clauses ("during your turn") before matching
        let (base, during_your_turn) =
            if let Some(stripped) = rest.strip_suffix(" during your turn") {
                (stripped, true)
            } else {
                (rest, false)
            };

        let Some(subject_text) = base
            .strip_suffix(" leave your graveyard")
            .or_else(|| base.strip_suffix(" leaves your graveyard"))
        else {
            continue;
        };

        // Parse subject type filter: "creature cards", "artifact and/or creature cards", "cards"
        let filter = if subject_text == "cards" {
            None
        } else if let Some(type_text) = subject_text.strip_suffix(" cards") {
            // Handle "artifact and/or creature" → OR filter
            if type_text.contains(" and/or ") {
                let parts: Vec<&str> = type_text.split(" and/or ").collect();
                let filters: Vec<TargetFilter> = parts
                    .iter()
                    .filter_map(|part| {
                        let (f, rem) = parse_type_phrase(part.trim());
                        if rem.trim().is_empty() {
                            Some(f)
                        } else {
                            None
                        }
                    })
                    .collect();
                if filters.len() == parts.len() && filters.len() > 1 {
                    Some(TargetFilter::Or { filters })
                } else {
                    continue;
                }
            } else {
                let (filter, remainder) = parse_type_phrase(type_text);
                if !remainder.trim().is_empty() {
                    continue;
                }
                Some(filter)
            }
        } else {
            continue;
        };

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZoneAll;
        def.origin = Some(Zone::Graveyard);
        def.valid_card = filter;
        def.batched = true;
        // LTB-from-graveyard triggers need to fire from graveyard zone context
        def.trigger_zones = vec![Zone::Battlefield, Zone::Graveyard, Zone::Exile];
        if during_your_turn {
            def.constraint = Some(TriggerConstraint::OnlyDuringYourTurn);
        }
        return Some((TriggerMode::ChangesZoneAll, def));
    }

    None
}

fn try_parse_one_or_more_combat_damage_to_player(
    lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever one or more ", "when one or more "] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        let Some(subject_text) = rest
            .strip_suffix(" deal combat damage to a player")
            .or_else(|| rest.strip_suffix(" deals combat damage to a player"))
        else {
            continue;
        };

        let (filter, remainder) = parse_type_phrase(subject_text);
        let filter = if remainder.trim().is_empty() {
            filter
        } else if let Some(or_filter) = try_split_or_compound_type_phrase(subject_text) {
            // CR 205.3m: Handle "ninja or rogue creatures you control" compound subtypes
            or_filter
        } else {
            continue;
        };

        let mut def = make_base();
        def.mode = TriggerMode::DamageDoneOnceByController;
        def.damage_kind = DamageKindFilter::CombatOnly;
        def.valid_source = Some(filter);
        def.valid_target = Some(TargetFilter::Player);
        return Some((TriggerMode::DamageDoneOnceByController, def));
    }

    None
}

/// CR 205.3m: Try to split "subtype or subtype [card_type] [you control]" into an Or filter.
/// Handles patterns like "ninja or rogue creatures you control" where parse_type_phrase
/// can't natively handle the "or" compound with a shared card_type suffix.
/// Parses the full right-side phrase ("rogue creatures you control") as a complete type phrase,
/// then applies the shared card_type and controller to the left-side bare subtype.
fn try_split_or_compound_type_phrase(text: &str) -> Option<TargetFilter> {
    let (left, right) = text.split_once(" or ")?;
    let left_trimmed = left.trim();
    // Parse the full right side as a type phrase — "rogue creatures you control" is a complete phrase
    // that parse_type_phrase handles as subtype-only + trailing text. Instead, parse the whole
    // "subtype card_type controller" suffix manually by feeding "right" to parse_type_phrase
    // but appending it to make a single-subtype phrase.
    // The simplest correct approach: parse the entire text AFTER stripping the "subtype or " prefix
    // from the left, treating the rest as a single type phrase that gives us card_type + controller.
    let right_trimmed = right.trim();
    // Try parsing the entire right side as a type phrase
    let (right_filter, right_remainder) = parse_type_phrase(right_trimmed);
    // If parse_type_phrase didn't fully consume, the right side has "subtype card_type you control"
    // pattern. Reconstruct: the right_filter has subtype, and remainder has "card_type you control".
    let (primary_type, controller) = if right_remainder.trim().is_empty() {
        // Fully consumed
        if let TargetFilter::Typed(ref tf) = right_filter {
            (tf.get_primary_type().cloned(), tf.controller.clone())
        } else {
            return None;
        }
    } else if let TargetFilter::Typed(ref tf) = right_filter {
        // Partially consumed: right_filter has subtype, remainder has "creatures you control"
        let (suffix_filter, suffix_rem) = parse_type_phrase(right_remainder.trim());
        if !suffix_rem.trim().is_empty() {
            return None;
        }
        if let TargetFilter::Typed(ref stf) = suffix_filter {
            (
                stf.get_primary_type()
                    .cloned()
                    .or(tf.get_primary_type().cloned()),
                stf.controller.clone().or(tf.controller.clone()),
            )
        } else {
            return None;
        }
    } else {
        return None;
    };
    // Extract right-side subtype
    let right_subtype = if let TargetFilter::Typed(ref tf) = right_filter {
        tf.get_subtype().map(|s| s.to_string())
    } else {
        return None;
    };
    // CR 205.3m: Canonicalize the left subtype (e.g. "ninjas" → "Ninja", "elves" → "Elf")
    let left_subtype = parse_subtype(left_trimmed)
        .map(|(canonical, _)| canonical)
        .unwrap_or_else(|| canonicalize_subtype_name(left_trimmed));
    let mut left_tf = TypedFilter::default().subtype(left_subtype);
    let mut right_tf = TypedFilter::default();
    if let Some(ref pt) = primary_type {
        left_tf = left_tf.with_type(pt.clone());
        right_tf = right_tf.with_type(pt.clone());
    }
    if let Some(rs) = right_subtype {
        right_tf = right_tf.subtype(rs);
    }
    left_tf.controller = controller.clone();
    right_tf.controller = controller;
    let filters = vec![TargetFilter::Typed(left_tf), TargetFilter::Typed(right_tf)];
    Some(TargetFilter::Or { filters })
}

fn try_parse_self_or_another_controlled_subtype_enters(
    lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever ~ or another ", "when ~ or another "] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        let Some(subject_text) = rest
            .strip_suffix(" enters")
            .or_else(|| rest.strip_suffix(" enters the battlefield"))
        else {
            continue;
        };
        let Some(subtype_text) = subject_text.trim().strip_suffix(" you control") else {
            continue;
        };
        let (_, remainder) = parse_type_phrase(subtype_text);
        if remainder.len() < subtype_text.len() {
            continue;
        }
        if !is_subtype_phrase(subtype_text) {
            continue;
        }

        let Some(subtype_filters) =
            build_controlled_subtype_filters(subtype_text, true, ControllerRef::You)
        else {
            continue;
        };
        if subtype_filters.is_empty() {
            continue;
        }

        let mut filters = vec![TargetFilter::SelfRef];
        filters.extend(subtype_filters);

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.destination = Some(Zone::Battlefield);
        def.valid_card = Some(TargetFilter::Or { filters });
        return Some((TriggerMode::ChangesZone, def));
    }

    None
}

fn try_parse_another_controlled_subtype_enters(
    lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever another ", "when another "] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        let Some(subject_text) = rest
            .strip_suffix(" enters")
            .or_else(|| rest.strip_suffix(" enters the battlefield"))
        else {
            continue;
        };
        let Some(subtype_text) = subject_text.trim().strip_suffix(" you control") else {
            continue;
        };
        let (_, remainder) = parse_type_phrase(subtype_text);
        if remainder.len() < subtype_text.len() {
            continue;
        }
        if !is_subtype_phrase(subtype_text) {
            continue;
        }

        let valid_card = build_controlled_subtype_filter(subtype_text, true, ControllerRef::You)?;

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.destination = Some(Zone::Battlefield);
        def.valid_card = Some(valid_card);
        return Some((TriggerMode::ChangesZone, def));
    }

    None
}

fn try_parse_controlled_subtype_attacks(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever a ", "whenever an ", "when a ", "when an "] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        let Some(subject_text) = rest.strip_suffix(" attacks") else {
            continue;
        };
        let Some(subtype_text) = subject_text.trim().strip_suffix(" you control") else {
            continue;
        };
        let (_, remainder) = parse_type_phrase(subtype_text);
        if remainder.len() < subtype_text.len() {
            continue;
        }
        if !is_subtype_phrase(subtype_text) {
            continue;
        }

        let valid_card = build_controlled_subtype_filter(subtype_text, false, ControllerRef::You)?;

        let mut def = make_base();
        def.mode = TriggerMode::Attacks;
        def.valid_card = Some(valid_card);
        return Some((TriggerMode::Attacks, def));
    }

    None
}

fn is_core_type_name(text: &str) -> bool {
    matches!(
        text,
        "creature"
            | "artifact"
            | "enchantment"
            | "land"
            | "planeswalker"
            | "spell"
            | "card"
            | "permanent"
    )
}

fn is_non_subtype_subject_name(text: &str) -> bool {
    matches!(
        text,
        "ability"
            | "card"
            | "commander"
            | "opponent"
            | "permanent"
            | "player"
            | "source"
            | "spell"
            | "token"
    )
}

fn is_subtype_phrase(text: &str) -> bool {
    text.split(" or ").all(|part| {
        let trimmed = part.trim();
        !trimmed.is_empty() && !is_core_type_name(trimmed) && !is_non_subtype_subject_name(trimmed)
    })
}

fn build_controlled_subtype_filter(
    subtype_text: &str,
    another: bool,
    controller: ControllerRef,
) -> Option<TargetFilter> {
    let filters = build_controlled_subtype_filters(subtype_text, another, controller)?;
    Some(match filters.as_slice() {
        [single] => single.clone(),
        _ => TargetFilter::Or { filters },
    })
}

fn build_controlled_subtype_filters(
    subtype_text: &str,
    another: bool,
    controller: ControllerRef,
) -> Option<Vec<TargetFilter>> {
    let mut filters = Vec::new();

    for subtype in subtype_text
        .split(" or ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if is_core_type_name(subtype) || is_non_subtype_subject_name(subtype) {
            return None;
        }

        let mut typed = TypedFilter::default()
            .subtype(canonicalize_subtype_name(subtype))
            .controller(controller.clone());
        if another {
            typed = typed.properties(vec![FilterProp::Another]);
        }
        filters.push(TargetFilter::Typed(typed));
    }

    if filters.is_empty() {
        None
    } else {
        Some(filters)
    }
}

// ---------------------------------------------------------------------------
// Category parsers
// ---------------------------------------------------------------------------

/// Parse phase triggers: "At the beginning of your upkeep/end step/combat/draw step"
fn try_parse_phase_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // CR 511.2: "at end of combat" triggers as the end of combat step begins.
    if let Some(rest) = lower
        .strip_prefix("at end of combat")
        .or_else(|| lower.strip_prefix("at the end of combat"))
    {
        let mut def = make_base();
        def.mode = TriggerMode::Phase;
        def.phase = Some(Phase::EndCombat);
        // CR 511.2: "on your turn" restricts to active player's combat.
        let rest = rest.trim();
        if rest.starts_with("on your turn") || rest.starts_with("on each of your turns") {
            def.constraint = Some(TriggerConstraint::OnlyDuringYourTurn);
        }
        return Some((TriggerMode::Phase, def));
    }

    let stripped = lower.strip_prefix("at the beginning of")?;
    let phase_text = stripped.trim();
    let mut def = make_base();
    def.mode = TriggerMode::Phase;
    if phase_text.contains("upkeep") {
        def.phase = Some(Phase::Upkeep);
    } else if phase_text.contains("end step") {
        // CR 513.1: End step triggers fire at the beginning of the end step.
        def.phase = Some(Phase::End);
    } else if phase_text.contains("postcombat main phase")
        || phase_text.contains("second main phase")
    {
        // CR 505.1: Postcombat main phase follows the combat phase.
        def.phase = Some(Phase::PostCombatMain);
    } else if phase_text.contains("precombat main phase") || phase_text.contains("first main phase")
    {
        // CR 505.1: Precombat main phase precedes the combat phase.
        def.phase = Some(Phase::PreCombatMain);
    } else if phase_text.contains("combat") {
        def.phase = Some(Phase::BeginCombat);
    } else if phase_text.contains("draw step") {
        def.phase = Some(Phase::Draw);
    }

    // CR 503.1a / CR 507.1: Parse possessive qualifier for turn constraint.
    // IMPORTANT: Check "opponent" before bare "your" — "your opponents'" contains "your"
    if phase_text.contains("each opponent") || phase_text.contains("your opponent") {
        def.constraint = Some(TriggerConstraint::OnlyDuringOpponentsTurn);
    } else if phase_text.contains("your") {
        def.constraint = Some(TriggerConstraint::OnlyDuringYourTurn);
    }
    // "each player's upkeep" / "each upkeep" / "the end step" → no constraint (fires every turn)

    Some((TriggerMode::Phase, def))
}

/// Parse player-centric triggers: "you gain life", "you cast a/an ...", "you draw a card"
fn try_parse_player_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    if lower.contains("you gain life") {
        let mut def = make_base();
        def.mode = TriggerMode::LifeGained;
        return Some((TriggerMode::LifeGained, def));
    }

    // "whenever you cast your Nth spell each turn" — must precede generic "you cast a"
    if let Some(result) = try_parse_nth_spell_trigger(lower) {
        return Some(result);
    }

    // "whenever you draw your Nth card each turn" — must precede generic "you draw a card"
    if let Some(result) = try_parse_nth_draw_trigger(lower) {
        return Some(result);
    }

    // CR 700.14: "whenever you expend N" — cumulative mana spent on spells this turn
    for prefix in ["whenever you expend ", "when you expend "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            if let Some((n, _)) = parse_number(rest) {
                let mut def = make_base();
                def.mode = TriggerMode::ManaExpend;
                def.expend_threshold = Some(n);
                return Some((TriggerMode::ManaExpend, def));
            }
        }
    }

    if matches!(
        lower,
        "whenever you discard a card" | "when you discard a card"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::Discarded;
        def.valid_card = Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Card).controller(ControllerRef::You),
        ));
        return Some((TriggerMode::Discarded, def));
    }

    if matches!(
        lower,
        "whenever an opponent discards a card" | "when an opponent discards a card"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::Discarded;
        def.valid_card = Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Card).controller(ControllerRef::Opponent),
        ));
        return Some((TriggerMode::Discarded, def));
    }

    // CR 603.10c: Batched discard triggers — "one or more" fire once per batch.
    if matches!(
        lower,
        "whenever you discard one or more cards" | "when you discard one or more cards"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::DiscardedAll;
        def.valid_target = Some(TargetFilter::Controller);
        def.batched = true;
        return Some((TriggerMode::DiscardedAll, def));
    }

    if matches!(
        lower,
        "whenever one or more players discard one or more cards"
            | "when one or more players discard one or more cards"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::DiscardedAll;
        def.batched = true;
        return Some((TriggerMode::DiscardedAll, def));
    }

    if matches!(
        lower,
        "whenever you sacrifice another permanent" | "when you sacrifice another permanent"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::Sacrificed;
        def.valid_card = Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Permanent)
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Another]),
        ));
        return Some((TriggerMode::Sacrificed, def));
    }

    if matches!(
        lower,
        "whenever you sacrifice a permanent" | "when you sacrifice a permanent"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::Sacrificed;
        def.valid_card = Some(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Permanent).controller(ControllerRef::You),
        ));
        return Some((TriggerMode::Sacrificed, def));
    }

    if matches!(
        lower,
        "whenever a player cycles a card" | "when a player cycles a card"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::Cycled;
        return Some((TriggerMode::Cycled, def));
    }

    if matches!(lower, "whenever you cycle a card" | "when you cycle a card") {
        let mut def = make_base();
        def.mode = TriggerMode::Cycled;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::Cycled, def));
    }

    if matches!(
        lower,
        "whenever an opponent draws a card" | "when an opponent draws a card"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));
        return Some((TriggerMode::Drawn, def));
    }

    // CR 701.21: "you tap an untapped creature an opponent controls"
    for prefix in [
        "whenever you tap an untapped creature an opponent controls",
        "when you tap an untapped creature an opponent controls",
    ] {
        if lower == prefix {
            let mut def = make_base();
            def.mode = TriggerMode::Taps;
            def.valid_card = Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::Opponent),
            ));
            return Some((TriggerMode::Taps, def));
        }
    }

    for prefix in ["whenever you tap ", "when you tap "] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        let Some(subject_text) = rest.strip_suffix(" for mana") else {
            continue;
        };
        let (filter, remainder) = parse_trigger_subject(subject_text);
        if !remainder.trim().is_empty() {
            continue;
        }

        let mut def = make_base();
        def.mode = TriggerMode::TapsForMana;
        def.valid_card = Some(filter);
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::TapsForMana, def));
    }

    for prefix in ["whenever a player taps ", "when a player taps "] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        let Some(subject_text) = rest.strip_suffix(" for mana") else {
            continue;
        };
        let (filter, remainder) = parse_trigger_subject(subject_text);
        if !remainder.trim().is_empty() {
            continue;
        }

        let mut def = make_base();
        def.mode = TriggerMode::TapsForMana;
        def.valid_card = Some(filter);
        return Some((TriggerMode::TapsForMana, def));
    }

    if matches!(lower, "whenever you lose life" | "when you lose life") {
        let mut def = make_base();
        def.mode = TriggerMode::LifeLost;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::LifeLost, def));
    }

    if matches!(
        lower,
        "whenever you lose life during your turn" | "when you lose life during your turn"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::LifeLost;
        def.valid_target = Some(TargetFilter::Controller);
        def.constraint = Some(TriggerConstraint::OnlyDuringYourTurn);
        return Some((TriggerMode::LifeLost, def));
    }

    for prefix in ["whenever you sacrifice ", "when you sacrifice "] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };
        let (filter, remainder) = parse_trigger_subject(rest);
        if !remainder.trim().is_empty() {
            continue;
        }

        let mut def = make_base();
        def.mode = TriggerMode::Sacrificed;
        def.valid_card = Some(add_controller(filter, ControllerRef::You));
        return Some((TriggerMode::Sacrificed, def));
    }

    // CR 601.2: "Whenever you cast a/an [type] spell" — extract the spell type filter.
    for prefix in ["you cast an ", "you cast a "] {
        if let Some(after) = strip_after(lower, prefix) {
            let mut def = make_base();
            def.mode = TriggerMode::SpellCast;
            // "you" = trigger's controller
            def.valid_target = Some(TargetFilter::Controller);

            // Parse the type phrase to extract a filter (e.g. "Aura spell", "creature spell").
            // TypeFilter::Card alone means "spell" with no type restriction — skip it.
            let (filter, _rest) = parse_type_phrase(after);
            let is_meaningful = match &filter {
                TargetFilter::Typed(tf) => tf.has_meaningful_type_constraint(),
                // Or-filters are always meaningful (e.g. "instant or sorcery spell")
                TargetFilter::Or { .. } => true,
                _ => false,
            };
            if is_meaningful {
                def.valid_card = Some(filter);
            }
            return Some((TriggerMode::SpellCast, def));
        }
    }

    // "an opponent casts a [quality] spell" / "a player casts a spell from a graveyard"
    if let Some(casts_pos) = lower.find(" casts a") {
        let who = &lower[..casts_pos];
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;

        // Determine the caster filter
        if who.contains("opponent") {
            def.valid_target = Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ));
        }

        // Parse the spell quality generically (e.g., "creature spell", "multicolored spell")
        // using the same parse_type_phrase building block as the "you cast" branch above.
        // Truncate at ", " to avoid passing the effect clause (e.g., ", you gain 1 life")
        // into parse_type_phrase where it would cause infinite recursion.
        let after_casts = &lower[casts_pos + " casts a".len()..].trim_start();
        let after_article = after_casts
            .strip_prefix("n ") // "an" → strip the trailing "n "
            .unwrap_or(after_casts)
            .trim_start();
        let spell_clause = after_article
            .split_once(", ")
            .map(|(before, _)| before)
            .unwrap_or(after_article);
        // Handle "multicolored" as a spell property (not a type phrase)
        if spell_clause.contains("multicolored") {
            def.valid_card = Some(TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::Multicolored]),
            ));
        } else {
            let (filter, _rest) = parse_type_phrase(spell_clause);
            let is_meaningful = match &filter {
                TargetFilter::Typed(tf) => tf.has_meaningful_type_constraint(),
                TargetFilter::Or { .. } => true,
                _ => false,
            };
            if is_meaningful {
                def.valid_card = Some(filter);
            }
        }

        return Some((TriggerMode::SpellCast, def));
    }

    if lower.contains("you draw a card") {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        return Some((TriggerMode::Drawn, def));
    }

    // "whenever you attack" — player-centric attack trigger
    if lower.contains("whenever you attack") || lower.contains("when you attack") {
        let mut def = make_base();
        def.mode = TriggerMode::YouAttack;
        return Some((TriggerMode::YouAttack, def));
    }

    // CR 707.10: "whenever you copy a spell" — fires when the player creates a copy of a spell.
    if matches!(lower, "whenever you copy a spell" | "when you copy a spell") {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCopy;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::SpellCopy, def));
    }

    // "when you cast this spell" — self-cast trigger (fires from stack)
    if lower.contains("when you cast this spell") || lower.contains("when ~ is cast") {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;
        def.valid_card = Some(TargetFilter::SelfRef);
        // Cast triggers fire while the spell is on the stack
        def.trigger_zones = vec![Zone::Stack];
        return Some((TriggerMode::SpellCast, def));
    }

    // "when you cycle this card" / "when you cycle ~" — cycling self-trigger
    // The card is in the graveyard by the time this trigger is checked.
    if lower.contains("you cycle this card") || lower.contains("you cycle ~") {
        let mut def = make_base();
        def.mode = TriggerMode::Cycled;
        def.valid_card = Some(TargetFilter::SelfRef);
        def.trigger_zones = vec![Zone::Graveyard];
        return Some((TriggerMode::Cycled, def));
    }

    // CR 120.1: "whenever you're dealt combat damage" — must precede generic "dealt damage"
    if matches!(
        lower,
        "whenever you're dealt combat damage" | "when you're dealt combat damage"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::DamageReceived;
        def.damage_kind = DamageKindFilter::CombatOnly;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::DamageReceived, def));
    }

    // CR 120.1: "whenever you're dealt damage"
    if matches!(
        lower,
        "whenever you're dealt damage" | "when you're dealt damage"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::DamageReceived;
        def.valid_target = Some(TargetFilter::Controller);
        return Some((TriggerMode::DamageReceived, def));
    }

    // CR 120.1b: "whenever an opponent is dealt noncombat damage"
    if matches!(
        lower,
        "whenever an opponent is dealt noncombat damage"
            | "when an opponent is dealt noncombat damage"
    ) {
        let mut def = make_base();
        def.mode = TriggerMode::DamageReceived;
        def.damage_kind = DamageKindFilter::NoncombatOnly;
        def.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));
        return Some((TriggerMode::DamageReceived, def));
    }

    None
}

/// Parse "whenever you cast your Nth spell each turn" (or "in a turn") and
/// "whenever an opponent casts their Nth [noncreature] spell each turn" into a SpellCast
/// trigger with a NthSpellThisTurn constraint.
fn try_parse_nth_spell_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // Branch 1: "you cast your <ordinal> [qualifier] spell each turn"
    if let Some(result) = try_parse_nth_spell_you(lower) {
        return Some(result);
    }
    // Branch 2: "an opponent casts their <ordinal> [qualifier] spell each turn"
    if let Some(result) = try_parse_nth_spell_opponent(lower) {
        return Some(result);
    }
    // Branch 3: "a player casts their <ordinal> [qualifier] spell each turn"
    if let Some(result) = try_parse_nth_spell_any_player(lower) {
        return Some(result);
    }
    None
}

/// "you cast your <ordinal> [qualifier] spell each turn"
/// Also handles "during each opponent's turn" variant (CR 601.2).
fn try_parse_nth_spell_you(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "you cast your ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    let filter = extract_spell_type_filter(rest);
    let after_qualifier = skip_to_word(rest, "spell");
    // CR 601.2: Standard "each turn" / "in a turn" patterns
    if after_qualifier.starts_with("spell each turn")
        || after_qualifier.starts_with("spell in a turn")
    {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;
        def.constraint = Some(TriggerConstraint::NthSpellThisTurn { n, filter });
        return Some((TriggerMode::SpellCast, def));
    }
    // CR 601.2: "during each opponent's turn" — same nth-spell tracking but only
    // during opponents' turns.
    if after_qualifier.starts_with("spell during each opponent's turn")
        || after_qualifier.starts_with("spell during each opponent\u{2019}s turn")
    {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;
        def.constraint = Some(TriggerConstraint::NthSpellThisTurn { n, filter });
        // Layer on the opponent's-turn constraint
        def.condition = Some(TriggerCondition::DuringOpponentsTurn);
        return Some((TriggerMode::SpellCast, def));
    }
    None
}

/// "an opponent casts their <ordinal> [qualifier] spell each turn"
fn try_parse_nth_spell_opponent(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "an opponent casts their ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    let filter = extract_spell_type_filter(rest);
    let after_qualifier = skip_to_word(rest, "spell");
    if after_qualifier.starts_with("spell each turn")
        || after_qualifier.starts_with("spell in a turn")
    {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;
        def.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));
        def.constraint = Some(TriggerConstraint::NthSpellThisTurn { n, filter });
        return Some((TriggerMode::SpellCast, def));
    }
    None
}

/// "a player casts their <ordinal> [qualifier] spell each turn"
/// CR 603.2: No valid_target filter — fires for any player's spell.
/// NthSpellThisTurn constraint extracts caster from the SpellCast event
/// and checks per-player counts via spells_cast_this_turn_by_player.
fn try_parse_nth_spell_any_player(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "a player casts their ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    let filter = extract_spell_type_filter(rest);
    let after_qualifier = skip_to_word(rest, "spell");
    if after_qualifier.starts_with("spell each turn")
        || after_qualifier.starts_with("spell in a turn")
    {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;
        def.constraint = Some(TriggerConstraint::NthSpellThisTurn { n, filter });
        return Some((TriggerMode::SpellCast, def));
    }
    None
}

/// Extract a type filter from the qualifier between ordinal and "spell".
/// e.g., "noncreature spell" → Some(TypedFilter with Non(Creature))
fn extract_spell_type_filter(after_ordinal: &str) -> Option<TypedFilter> {
    let trimmed = after_ordinal.trim();
    if let Some(before_spell) = trimmed
        .strip_suffix("spell each turn")
        .or_else(|| trimmed.strip_suffix("spell in a turn"))
        .or_else(|| trimmed.strip_suffix("spell during each opponent's turn"))
        .or_else(|| trimmed.strip_suffix("spell during each opponent\u{2019}s turn"))
    {
        let qualifier = before_spell.trim();
        if qualifier.eq_ignore_ascii_case("noncreature") {
            return Some(
                TypedFilter::new(TypeFilter::Card)
                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Creature))),
            );
        }
        // Future: "artifact", "instant or sorcery", etc.
    }
    None
}

/// Parse "whenever [subject] draw(s) [possessive] Nth card each turn" into a Drawn trigger
/// with a NthDrawThisTurn constraint.
/// Follows the same decomposition pattern as `try_parse_nth_spell_trigger`.
fn try_parse_nth_draw_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    if let Some(result) = try_parse_nth_draw_you(lower) {
        return Some(result);
    }
    if let Some(result) = try_parse_nth_draw_opponent(lower) {
        return Some(result);
    }
    if let Some(result) = try_parse_nth_draw_any_player(lower) {
        return Some(result);
    }
    None
}

/// "you draw your <ordinal> card each turn"
fn try_parse_nth_draw_you(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "you draw your ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    if rest.starts_with("card each turn") || rest.starts_with("card in a turn") {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.constraint = Some(TriggerConstraint::NthDrawThisTurn { n });
        return Some((TriggerMode::Drawn, def));
    }
    None
}

/// "an opponent draws their <ordinal> card each turn"
fn try_parse_nth_draw_opponent(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "an opponent draws their ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    if rest.starts_with("card each turn") || rest.starts_with("card in a turn") {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.valid_target = Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::Opponent),
        ));
        def.constraint = Some(TriggerConstraint::NthDrawThisTurn { n });
        return Some((TriggerMode::Drawn, def));
    }
    None
}

/// "a player draws their <ordinal> card each turn"
/// CR 121.2: No valid_target filter — fires for any player's draw.
fn try_parse_nth_draw_any_player(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    let prefix = "a player draws their ";
    let after = strip_after(lower, prefix)?;
    let (n, rest) = parse_ordinal(after)?;
    if rest.starts_with("card each turn") || rest.starts_with("card in a turn") {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        def.constraint = Some(TriggerConstraint::NthDrawThisTurn { n });
        return Some((TriggerMode::Drawn, def));
    }
    None
}

/// Skip past words before a target word, returning the text from that word onward.
/// If the target word is not found, returns the original text.
fn skip_to_word<'a>(text: &'a str, word: &str) -> &'a str {
    if let Some(pos) = text.find(word) {
        &text[pos..]
    } else {
        text
    }
}

/// Parse counter-placement triggers from Oracle text.
/// Handles all patterns: passive ("a counter is put on ~"), active ("you put counters on ~"),
/// and with arbitrary subjects ("counters are put on another creature you control").
fn try_parse_counter_trigger(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    if !lower.contains("counter") {
        return None;
    }

    // CR 121.6: "a [type] counter is removed from ~" — counter removal trigger.
    // Check removal before placement to avoid false-matching "removed" as "put".
    if let Some(result) = try_parse_counter_removed(lower) {
        return Some(result);
    }

    // Must mention both a counter and a placement verb
    if !lower.contains("put") && !lower.contains("placed") {
        return None;
    }

    // Find "counter(s) ... on SUBJECT" — locate " on " after the counter mention
    let counter_pos = lower.find("counter")?;
    let after_counter = &lower[counter_pos..];
    let on_offset = after_counter.find(" on ")?;
    let subject_start = counter_pos + on_offset + " on ".len();
    let subject_text = lower[subject_start..].trim();

    let mut def = make_base();
    def.mode = TriggerMode::CounterAdded;

    // Parse the subject after "on "
    if subject_text.starts_with('~') {
        def.valid_card = Some(TargetFilter::SelfRef);
    } else {
        let (filter, _) = parse_single_subject(subject_text);
        def.valid_card = Some(filter);
    }

    Some((TriggerMode::CounterAdded, def))
}

/// CR 121.6: Parse "a [type] counter is removed from [subject]" patterns.
/// Also handles zone constraints like "while it's exiled" (e.g. suspend cards).
fn try_parse_counter_removed(lower: &str) -> Option<(TriggerMode, TriggerDefinition)> {
    // Pattern: "a [type] counter is removed from [subject] [while ...]"
    let after_a = lower
        .strip_prefix("whenever a ")
        .or_else(|| lower.strip_prefix("when a "))?;

    let counter_pos = after_a.find(" counter is removed from ")?;
    let counter_type = after_a[..counter_pos].trim();
    let subject_start = counter_pos + " counter is removed from ".len();
    let subject_rest = after_a[subject_start..].trim();

    let mut def = make_base();
    def.mode = TriggerMode::CounterRemoved;

    // Parse optional "while it's exiled" / "while ~ is exiled" zone constraint
    let (subject_text, zone_constraint) =
        if let Some(before) = subject_rest.strip_suffix("while it's exiled") {
            (before.trim(), Some(Zone::Exile))
        } else if let Some(before) = subject_rest.strip_suffix("while ~ is exiled") {
            (before.trim(), Some(Zone::Exile))
        } else {
            (subject_rest, None)
        };

    // Parse subject
    if subject_text == "~" || subject_text == "this card" {
        def.valid_card = Some(TargetFilter::SelfRef);
    } else {
        let (filter, _) = parse_single_subject(subject_text);
        def.valid_card = Some(filter);
    }

    // Set counter type as description metadata (the counter_filter field could be extended
    // but for now the type info is captured in the description)
    if !counter_type.is_empty() {
        def.description = Some(format!("{counter_type} counter"));
    }

    // CR 121.6: Zone constraint for cards that trigger from exile (e.g. suspend)
    if let Some(zone) = zone_constraint {
        def.trigger_zones = vec![zone];
    }

    Some((TriggerMode::CounterRemoved, def))
}

/// CR 700.4: Parse "is/are put into [possessive] graveyard [from zone]" patterns.
/// Handles all forms:
/// - "is put into a graveyard from anywhere" (no origin restriction)
/// - "is put into a graveyard from the battlefield" (equivalent to "dies")
/// - "is put into your graveyard [from your library]" (controller filter + optional origin)
/// - "is put into an opponent's graveyard from anywhere" (opponent controller filter)
/// - "are put into your graveyard from your library" (plural form for batched triggers)
fn try_parse_put_into_graveyard(
    subject: &TargetFilter,
    rest: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    // Match the verb prefix: "is put into " or "are put into "
    let after_verb = rest
        .strip_prefix("is put into ")
        .or_else(|| rest.strip_prefix("are put into "))?;

    // Parse the graveyard possessive: "a graveyard", "your graveyard", "an opponent's graveyard"
    let (valid_target, after_gy) = if let Some(after) = after_verb.strip_prefix("a graveyard") {
        (None, after)
    } else if let Some(after) = after_verb.strip_prefix("your graveyard") {
        (
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
            after,
        )
    } else if let Some(after) = after_verb.strip_prefix("an opponent's graveyard") {
        (
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
            after,
        )
    } else {
        return None;
    };

    // Parse optional "from [zone]" clause
    let after_gy = after_gy.trim_start();
    let origin = if let Some(after_from) = after_gy.strip_prefix("from ") {
        let after_from = after_from.trim_start();
        if after_from.starts_with("the battlefield") {
            Some(Zone::Battlefield)
        } else if after_from.starts_with("anywhere") {
            // CR 700.4: "from anywhere" means no origin restriction
            None
        } else if after_from.starts_with("your library") {
            Some(Zone::Library)
        } else if after_from.starts_with("your hand") {
            Some(Zone::Hand)
        } else {
            // Unknown origin zone -- treat as no restriction
            None
        }
    } else {
        // No "from" clause -- no origin restriction (any zone to graveyard)
        None
    };

    let mut def = make_base();
    def.mode = TriggerMode::ChangesZone;
    def.destination = Some(Zone::Graveyard);
    def.origin = origin;
    def.valid_card = Some(subject.clone());
    def.valid_target = valid_target;
    Some((TriggerMode::ChangesZone, def))
}

/// Parse "whenever one or more [type] cards are put into [your] graveyard from [your library]".
/// CR 603.10c: "One or more" triggers fire once per batch of simultaneous events.
fn try_parse_one_or_more_put_into_graveyard(
    lower: &str,
) -> Option<(TriggerMode, TriggerDefinition)> {
    for prefix in ["whenever one or more ", "when one or more "] {
        let Some(rest) = lower.strip_prefix(prefix) else {
            continue;
        };

        // Find "are put into" / "is put into" to split subject from destination
        let put_into_pos = rest
            .find(" are put into ")
            .or_else(|| rest.find(" is put into "))?;
        let subject_text = &rest[..put_into_pos];
        let after_put =
            if let Some(p) = rest.strip_prefix(&rest[..put_into_pos + " are put into ".len()]) {
                p
            } else {
                &rest[put_into_pos + " is put into ".len()..]
            };

        // Parse the graveyard possessive
        let (valid_target, after_gy) = if let Some(after) = after_put.strip_prefix("a graveyard") {
            (None, after)
        } else if let Some(after) = after_put.strip_prefix("your graveyard") {
            (
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                after,
            )
        } else if let Some(after) = after_put.strip_prefix("an opponent's graveyard") {
            (
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                after,
            )
        } else {
            continue;
        };

        // Parse optional "from [zone]" clause
        let after_gy = after_gy.trim_start();
        let origin = if let Some(after_from) = after_gy.strip_prefix("from ") {
            let after_from = after_from.trim_start();
            if after_from.starts_with("the battlefield") {
                Some(Zone::Battlefield)
            } else if after_from.starts_with("anywhere") {
                None
            } else if after_from.starts_with("your library") {
                Some(Zone::Library)
            } else {
                None
            }
        } else {
            None
        };

        // Parse the subject type filter: "creature cards", "land cards", "cards"
        let filter = if subject_text == "cards" {
            None
        } else if let Some(type_text) = subject_text.strip_suffix(" cards") {
            let (f, remainder) = parse_type_phrase(type_text);
            if !remainder.trim().is_empty() {
                continue;
            }
            Some(f)
        } else {
            continue;
        };

        let mut def = make_base();
        def.mode = TriggerMode::ChangesZoneAll;
        def.destination = Some(Zone::Graveyard);
        def.origin = origin;
        def.valid_card = filter;
        def.valid_target = valid_target;
        def.batched = true;
        return Some((TriggerMode::ChangesZoneAll, def));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Duration, Effect, PtValue, QuantityExpr, QuantityRef, UnlessCost};

    #[test]
    fn trigger_etb_self() {
        let def = parse_trigger_line(
            "When this creature enters, it deals 1 damage to each opponent.",
            "Goblin Chainwhirler",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_dies() {
        let def = parse_trigger_line(
            "When this creature dies, create a 1/1 white Spirit creature token.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
    }

    #[test]
    fn trigger_combat_damage_to_player() {
        let def = parse_trigger_line(
            "Whenever Eye Collector deals combat damage to a player, each player mills a card.",
            "Eye Collector",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    }

    #[test]
    fn trigger_one_or_more_creatures_you_control_deal_combat_damage_to_player() {
        let def = parse_trigger_line(
            "Whenever one or more creatures you control deal combat damage to a player, create a Treasure token.",
            "Professional Face-Breaker",
        );
        assert_eq!(def.mode, TriggerMode::DamageDoneOnceByController);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(
            def.valid_source,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You)
            ))
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Player));
    }

    #[test]
    fn trigger_upkeep() {
        let def = parse_trigger_line(
            "At the beginning of your upkeep, look at the top card of your library.",
            "Delver of Secrets",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
    }

    #[test]
    fn trigger_optional_you_may() {
        let def = parse_trigger_line(
            "When this creature enters, you may draw a card.",
            "Some Card",
        );
        assert!(def.optional);
    }

    #[test]
    fn trigger_attacks() {
        let def = parse_trigger_line(
            "Whenever Goblin Guide attacks, defending player reveals the top card of their library.",
            "Goblin Guide",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
    }

    #[test]
    fn trigger_battalion() {
        let def = parse_trigger_line(
            "Whenever Boros Elite and at least two other creatures attack, Boros Elite gets +2/+2 until end of turn.",
            "Boros Elite",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert!(def.condition.is_some());
        if let Some(TriggerCondition::MinCoAttackers { minimum }) = &def.condition {
            assert_eq!(*minimum, 2);
        } else {
            panic!("Expected MinCoAttackers");
        }
    }

    #[test]
    fn trigger_pack_tactics() {
        let def = parse_trigger_line(
            "Whenever Werewolf Pack Leader attacks, if the total power of creatures you control is 6 or greater, draw a card.",
            "Werewolf Pack Leader",
        );
        // Pack tactics is a different pattern (if-condition), not battalion
        assert_eq!(def.mode, TriggerMode::Attacks);
    }

    #[test]
    fn trigger_exploits_a_creature() {
        let def = parse_trigger_line(
            "When Sidisi's Faithful exploits a creature, return target creature to its owner's hand.",
            "Sidisi's Faithful",
        );
        assert_eq!(def.mode, TriggerMode::Exploited);
    }

    // --- Subject decomposition tests ---

    #[test]
    fn trigger_another_creature_you_control_enters() {
        let def = parse_trigger_line(
            "Whenever another creature you control enters, put a +1/+1 counter on Hinterland Sanctifier.",
            "Hinterland Sanctifier",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(crate::types::ability::ControllerRef::You)
                    .properties(vec![FilterProp::Another])
            ))
        );
    }

    #[test]
    fn trigger_another_creature_enters_no_controller() {
        let def = parse_trigger_line(
            "Whenever another creature enters, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        match &def.valid_card {
            Some(TargetFilter::Typed(TypedFilter { properties, .. })) => {
                assert!(properties.contains(&FilterProp::Another));
            }
            other => panic!("Expected Typed filter with Another, got {:?}", other),
        }
    }

    #[test]
    fn trigger_a_creature_enters() {
        let def = parse_trigger_line(
            "Whenever a creature enters, you gain 1 life.",
            "Soul Warden",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::creature()))
        );
    }

    #[test]
    fn trigger_counter_put_on_self() {
        let def = parse_trigger_line(
            "Whenever a +1/+1 counter is put on ~, draw a card.",
            "Fathom Mage",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_one_or_more_counters_on_self() {
        let def = parse_trigger_line(
            "Whenever one or more counters are put on ~, you gain 1 life.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    // --- Constraint parsing tests ---

    #[test]
    fn trigger_once_each_turn_constraint() {
        let def = parse_trigger_line(
            "Whenever you gain life, put a +1/+1 counter on Exemplar of Light. This ability triggers only once each turn.",
            "Exemplar of Light",
        );
        assert_eq!(def.mode, TriggerMode::LifeGained);
        assert_eq!(
            def.constraint,
            Some(crate::types::ability::TriggerConstraint::OncePerTurn)
        );
    }

    #[test]
    fn trigger_no_constraint_by_default() {
        let def = parse_trigger_line(
            "Whenever you gain life, put a +1/+1 counter on this creature.",
            "Ajani's Pridemate",
        );
        assert_eq!(def.mode, TriggerMode::LifeGained);
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_only_during_your_turn() {
        let def = parse_trigger_line(
            "Whenever a creature enters, draw a card. This ability triggers only during your turn.",
            "Some Card",
        );
        assert_eq!(
            def.constraint,
            Some(crate::types::ability::TriggerConstraint::OnlyDuringYourTurn)
        );
    }

    // --- Compound subject tests ---

    #[test]
    fn trigger_self_or_another_creature_or_artifact_you_control() {
        use crate::types::ability::{ControllerRef, TypeFilter};
        let def = parse_trigger_line(
            "Whenever Haliya or another creature or artifact you control enters, you gain 1 life.",
            "Haliya, Guided by Light",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        match &def.valid_card {
            Some(TargetFilter::Or { filters }) => {
                assert_eq!(filters.len(), 3);
                assert_eq!(filters[0], TargetFilter::SelfRef);
                // Both branches should have Another + You controller
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::Another])
                    )
                );
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact)
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::Another])
                    )
                );
            }
            other => panic!("Expected Or filter with 3 branches, got {:?}", other),
        }
    }

    #[test]
    fn normalize_legendary_short_name() {
        let result = normalize_self_refs(
            "Whenever Haliya or another creature enters",
            "Haliya, Guided by Light",
        );
        assert_eq!(result, "Whenever ~ or another creature enters");
    }

    #[test]
    fn trigger_first_word_short_name_enters() {
        let def = parse_trigger_line(
            "When Sharuum enters, you may return target artifact card from your graveyard to the battlefield.",
            "Sharuum the Hegemon",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert!(def.optional);
    }

    #[test]
    fn trigger_a_prefix_card_enters() {
        let def = parse_trigger_line(
            "When Sprouting Goblin enters, search your library for a land card with a basic land type, reveal it, put it into your hand, then shuffle.",
            "A-Sprouting Goblin",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
    }

    #[test]
    fn trigger_self_or_another_creature_enters() {
        let def = parse_trigger_line(
            "Whenever Some Card or another creature enters, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        match &def.valid_card {
            Some(TargetFilter::Or { filters }) => {
                assert_eq!(filters.len(), 2);
                assert_eq!(filters[0], TargetFilter::SelfRef);
                match &filters[1] {
                    TargetFilter::Typed(TypedFilter { properties, .. }) => {
                        assert!(properties.contains(&FilterProp::Another));
                    }
                    other => panic!("Expected Typed with Another, got {:?}", other),
                }
            }
            other => panic!("Expected Or filter, got {:?}", other),
        }
    }

    // --- Intervening-if condition tests ---

    #[test]
    fn trigger_haliya_end_step_with_life_condition() {
        let def = parse_trigger_line(
            "At the beginning of your end step, draw a card if you've gained 3 or more life this turn.",
            "Haliya, Guided by Light",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::GainedLife { minimum: 3 })
        );
        // Effect should be just "draw a card" with condition stripped
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_if_gained_life_no_number() {
        let def = parse_trigger_line(
            "At the beginning of your end step, create a Blood token if you gained life this turn.",
            "Some Card",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::GainedLife { minimum: 1 })
        );
    }

    #[test]
    fn trigger_if_descended_this_turn() {
        let def = parse_trigger_line(
            "At the beginning of your end step, if you descended this turn, scry 1.",
            "Ruin-Lurker Bat",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        assert_eq!(def.condition, Some(TriggerCondition::Descended));
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_if_gained_5_or_more_life() {
        let def = parse_trigger_line(
            "At the beginning of each end step, if you gained 5 or more life this turn, create a 4/4 white Angel creature token with flying and vigilance.",
            "Resplendent Angel",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::GainedLife { minimum: 5 })
        );
        // Regression: execute must not be None — the effect text after the condition
        // must be preserved and parsed (previously the condition clause consumed the
        // entire text, leaving execute as None).
        assert!(
            def.execute.is_some(),
            "execute must be Some — effect text after 'if you gained N or more life this turn' was dropped"
        );
    }

    #[test]
    fn trigger_if_gained_4_or_more_life_angelic_accord() {
        // Angelic Accord: condition at start of effect text
        let def = parse_trigger_line(
            "At the beginning of each end step, if you gained 4 or more life this turn, create a 4/4 white Angel creature token with flying.",
            "Angelic Accord",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::GainedLife { minimum: 4 })
        );
        assert!(
            def.execute.is_some(),
            "execute must be Some — token creation effect was dropped"
        );
    }

    #[test]
    fn trigger_if_gained_life_this_turn_no_minimum() {
        // Ocelot Pride: "if you gained life this turn" (no number)
        let def = parse_trigger_line(
            "At the beginning of your end step, if you gained life this turn, create a 1/1 white Cat creature token.",
            "Ocelot Pride",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::GainedLife { minimum: 1 })
        );
        assert!(
            def.execute.is_some(),
            "execute must be Some — token creation effect was dropped"
        );
    }

    #[test]
    fn extract_if_strips_condition_from_effect() {
        let (cleaned, cond) =
            extract_if_condition("draw a card if you've gained 3 or more life this turn.");
        assert_eq!(cleaned, "draw a card");
        assert_eq!(cond, Some(TriggerCondition::GainedLife { minimum: 3 }));
    }

    #[test]
    fn trigger_if_gained_and_lost_life_compound() {
        let def = parse_trigger_line(
            "At the beginning of your end step, if you gained and lost life this turn, create a 1/1 black Bat creature token with flying.",
            "Some Card",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::And {
                conditions: vec![
                    TriggerCondition::GainedLife { minimum: 1 },
                    TriggerCondition::LostLife,
                ]
            })
        );
        assert!(def.execute.is_some());
    }

    // --- Counter placement with "you put" pattern ---

    #[test]
    fn trigger_you_put_counters_on_self() {
        let def = parse_trigger_line(
            "Whenever you put one or more +1/+1 counters on this creature, draw a card. This ability triggers only once each turn.",
            "Exemplar of Light",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.constraint,
            Some(crate::types::ability::TriggerConstraint::OncePerTurn)
        );
        // Constraint sentence should NOT leak as a sub-ability
        if let Some(ref exec) = def.execute {
            assert!(
                !matches!(
                    *exec.effect,
                    crate::types::ability::Effect::Unimplemented { .. }
                ),
                "Effect should be Draw, not Unimplemented"
            );
            assert!(
                exec.sub_ability.is_none(),
                "No spurious sub-ability from constraint text"
            );
        }
    }

    #[test]
    fn trigger_counters_put_on_another_creature_you_control() {
        use crate::types::ability::ControllerRef;
        let def = parse_trigger_line(
            "Whenever one or more +1/+1 counters are put on another creature you control, put a +1/+1 counter on this creature.",
            "Enduring Scalelord",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Another])
            ))
        );
    }

    #[test]
    fn trigger_you_put_counters_on_creature_you_control() {
        use crate::types::ability::ControllerRef;
        let def = parse_trigger_line(
            "Whenever you put one or more +1/+1 counters on a creature you control, draw a card.",
            "The Powerful Dragon",
        );
        assert_eq!(def.mode, TriggerMode::CounterAdded);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You)
            ))
        );
    }

    #[test]
    fn strip_constraint_does_not_affect_effect() {
        let result =
            strip_constraint_sentences("draw a card. this ability triggers only once each turn.");
        assert_eq!(result, "draw a card");
    }

    #[test]
    fn strip_constraint_preserves_plain_effect() {
        let result = strip_constraint_sentences("put a +1/+1 counter on ~");
        assert_eq!(result, "put a +1/+1 counter on ~");
    }

    // --- Color-filtered trigger subjects ---

    #[test]
    fn trigger_white_creature_you_control_attacks() {
        let def = parse_trigger_line(
            "Whenever a white creature you control attacks, you gain 1 life.",
            "Linden, the Steadfast Queen",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(crate::types::ability::ControllerRef::You)
                    .properties(vec![FilterProp::HasColor {
                        color: "White".to_string()
                    }])
            ))
        );
    }

    // --- New trigger mode tests ---

    #[test]
    fn trigger_land_enters() {
        let def = parse_trigger_line("When this land enters, you gain 1 life.", "Bloodfell Caves");
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_aura_enters() {
        let def = parse_trigger_line(
            "When this Aura enters, tap target creature an opponent controls.",
            "Glaring Aegis",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_equipment_enters() {
        let def = parse_trigger_line(
            "When this Equipment enters, attach it to target creature you control.",
            "Shining Armor",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_vehicle_enters() {
        let def = parse_trigger_line(
            "When this Vehicle enters, create a 1/1 white Pilot creature token.",
            "Some Vehicle",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_leaves_battlefield() {
        let def = parse_trigger_line(
            "When Oblivion Ring leaves the battlefield, return the exiled card to the battlefield.",
            "Oblivion Ring",
        );
        assert_eq!(def.mode, TriggerMode::LeavesBattlefield);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Graveyard));
        assert!(def.trigger_zones.contains(&Zone::Exile));
    }

    #[test]
    fn trigger_becomes_blocked() {
        let def = parse_trigger_line(
            "Whenever Gustcloak Cavalier becomes blocked, you may untap it and remove it from combat.",
            "Gustcloak Cavalier",
        );
        assert_eq!(def.mode, TriggerMode::BecomesBlocked);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_is_dealt_damage() {
        let def = parse_trigger_line(
            "Whenever Spitemare is dealt damage, it deals that much damage to any target.",
            "Spitemare",
        );
        assert_eq!(def.mode, TriggerMode::DamageReceived);
        assert_eq!(def.damage_kind, DamageKindFilter::Any);
    }

    #[test]
    fn trigger_is_dealt_combat_damage() {
        let def = parse_trigger_line(
            "Whenever ~ is dealt combat damage, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::DamageReceived);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    }

    #[test]
    fn trigger_you_attack() {
        let def = parse_trigger_line(
            "Whenever you attack, create a 1/1 white Soldier creature token.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
    }

    #[test]
    fn trigger_becomes_tapped() {
        let def = parse_trigger_line(
            "Whenever Night Market Lookout becomes tapped, each opponent loses 1 life and you gain 1 life.",
            "Night Market Lookout",
        );
        assert_eq!(def.mode, TriggerMode::Taps);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_you_cast_this_spell() {
        let def = parse_trigger_line(
            "When you cast this spell, draw cards equal to the greatest power among creatures you control.",
            "Hydroid Krasis",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Stack));
    }

    #[test]
    fn trigger_opponent_casts_multicolored_spell() {
        let def = parse_trigger_line(
            "Whenever an opponent casts a multicolored spell, you gain 1 life.",
            "Soldier of the Pantheon",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::Multicolored])
            ))
        );
    }

    #[test]
    fn trigger_you_cast_aura_spell() {
        let def = parse_trigger_line(
            "Whenever you cast an Aura spell, you may draw a card.",
            "Kor Spiritdancer",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        // Must restrict to Aura subtype
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::default().subtype("Aura".to_string())
            ))
        );
        // Must restrict to controller's spells
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_cast_creature_spell() {
        let def = parse_trigger_line(
            "Whenever you cast a creature spell, draw a card.",
            "Beast Whisperer",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_cast_a_spell_no_type() {
        let def = parse_trigger_line("Whenever you cast a spell, add {C}.", "Conduit of Ruin");
        assert_eq!(def.mode, TriggerMode::SpellCast);
        // No type restriction
        assert!(def.valid_card.is_none());
        // But still restricted to controller
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    // --- ControlCreatures condition tests ---

    #[test]
    fn trigger_leonin_vanguard_control_creature_count() {
        let def = parse_trigger_line(
            "At the beginning of combat on your turn, if you control three or more creatures, this creature gets +1/+1 until end of turn and you gain 1 life.",
            "Leonin Vanguard",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::BeginCombat));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::ControlCreatures { minimum: 3 })
        );
        // Effect: pump self +1/+1 with life gain sub_ability
        let exec = def.execute.as_ref().expect("should have execute");
        assert!(matches!(
            *exec.effect,
            Effect::Pump {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: TargetFilter::SelfRef,
            }
        ));
        assert_eq!(exec.duration, Some(Duration::UntilEndOfTurn));
        // Sub-ability: gain 1 life
        let sub = exec.sub_ability.as_ref().expect("should have sub_ability");
        assert!(matches!(
            *sub.effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
    }

    #[test]
    fn extract_if_control_creature_count() {
        let (cleaned, cond) = extract_if_condition(
            "if you control three or more creatures, ~ gets +1/+1 until end of turn",
        );
        assert_eq!(cleaned, "~ gets +1/+1 until end of turn");
        assert_eq!(
            cond,
            Some(TriggerCondition::ControlCreatures { minimum: 3 })
        );
    }

    // --- Equipment / Aura subject filter tests ---

    #[test]
    fn trigger_equipped_creature_attacks() {
        let def = parse_trigger_line(
            "Whenever equipped creature attacks, put a +1/+1 counter on it.",
            "Blackblade Reforged",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    }

    #[test]
    fn trigger_equipped_creature_deals_combat_damage() {
        let def = parse_trigger_line(
            "Whenever equipped creature deals combat damage to a player, draw a card.",
            "Shadowspear",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(def.valid_source, Some(TargetFilter::AttachedTo));
    }

    #[test]
    fn trigger_equipped_creature_dies() {
        let def = parse_trigger_line(
            "Whenever equipped creature dies, you gain 2 life.",
            "Strider Harness",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    }

    #[test]
    fn trigger_enchanted_creature_attacks() {
        let def = parse_trigger_line(
            "Whenever enchanted creature attacks, draw a card.",
            "Curiosity",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    }

    #[test]
    fn trigger_enchanted_creature_dies() {
        let def = parse_trigger_line(
            "Whenever enchanted creature dies, return ~ to its owner's hand.",
            "Angelic Destiny",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    }

    #[test]
    fn trigger_cycle_this_card() {
        let def = parse_trigger_line(
            "When you cycle this card, draw a card.",
            "Decree of Justice",
        );
        assert_eq!(def.mode, TriggerMode::Cycled);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Graveyard));
    }

    #[test]
    fn trigger_cycle_self_ref() {
        let def = parse_trigger_line(
            "When you cycle ~, you may draw a card.",
            "Decree of Justice",
        );
        assert_eq!(def.mode, TriggerMode::Cycled);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(def.trigger_zones.contains(&Zone::Graveyard));
        assert!(def.optional);
    }

    #[test]
    fn trigger_opponent_draws_a_card() {
        let def = parse_trigger_line(
            "Whenever an opponent draws a card, you gain 1 life.",
            "Underworld Dreams",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
    }

    #[test]
    fn trigger_you_cycle_a_card() {
        let def = parse_trigger_line("Whenever you cycle a card, draw a card.", "Drake Haven");
        assert_eq!(def.mode, TriggerMode::Cycled);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_lose_life() {
        let def = parse_trigger_line(
            "Whenever you lose life, create a 1/1 token.",
            "Unholy Annex",
        );
        assert_eq!(def.mode, TriggerMode::LifeLost);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_lose_life_during_your_turn() {
        let def = parse_trigger_line(
            "Whenever you lose life during your turn, draw a card.",
            "Bloodtracker",
        );
        assert_eq!(def.mode, TriggerMode::LifeLost);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn trigger_you_sacrifice_a_creature() {
        let def = parse_trigger_line(
            "Whenever you sacrifice a creature, draw a card.",
            "Morbid Opportunist",
        );
        assert_eq!(def.mode, TriggerMode::Sacrificed);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You)
            ))
        );
    }

    #[test]
    fn trigger_you_tap_a_land_for_mana() {
        let def = parse_trigger_line("Whenever you tap a land for mana, add {G}.", "Mana Flare");
        assert_eq!(def.mode, TriggerMode::TapsForMana);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)))
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_enchanted_land_is_tapped_for_mana() {
        let def = parse_trigger_line(
            "Whenever enchanted land is tapped for mana, its controller adds an additional {G}.",
            "Wild Growth",
        );
        assert_eq!(def.mode, TriggerMode::TapsForMana);
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));
    }

    #[test]
    fn trigger_nth_spell_second() {
        let def = parse_trigger_line(
            "Whenever you cast your second spell each turn, draw a card.",
            "Spectral Sailor",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthSpellThisTurn { n: 2, filter: None })
        );
    }

    #[test]
    fn trigger_nth_spell_third() {
        let def = parse_trigger_line(
            "Whenever you cast your third spell each turn, create a 1/1 token.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthSpellThisTurn { n: 3, filter: None })
        );
    }

    #[test]
    fn trigger_nth_draw_second() {
        let def = parse_trigger_line(
            "Whenever you draw your second card each turn, you gain 1 life.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthDrawThisTurn { n: 2 })
        );
    }

    #[test]
    fn trigger_nth_draw_opponent_second() {
        let def = parse_trigger_line(
            "Whenever an opponent draws their second card each turn, you draw two cards.",
            "The Unagi of Kyoshi Island",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            ))
        );
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthDrawThisTurn { n: 2 })
        );
    }

    #[test]
    fn trigger_nth_draw_any_player() {
        let def = parse_trigger_line(
            "Whenever a player draws their third card each turn, you gain 1 life.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::Drawn);
        assert_eq!(def.valid_target, None);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthDrawThisTurn { n: 3 })
        );
    }

    #[test]
    fn trigger_nth_spell_opponent_noncreature() {
        let def = parse_trigger_line(
            "Whenever an opponent casts their first noncreature spell each turn, draw a card.",
            "Esper Sentinel",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthSpellThisTurn {
                n: 1,
                filter: Some(
                    TypedFilter::new(TypeFilter::Card)
                        .with_type(TypeFilter::Non(Box::new(TypeFilter::Creature))),
                ),
            })
        );
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
    }

    #[test]
    fn trigger_esper_sentinel_unless_pay() {
        let def = parse_trigger_line(
            "Whenever an opponent casts their first noncreature spell each turn, draw a card unless that player pays {X}, where X is this creature's power.",
            "Esper Sentinel",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        // Effect should be Draw, not Unimplemented
        let execute = def.execute.as_ref().expect("should have execute");
        assert!(
            matches!(*execute.effect, Effect::Draw { .. }),
            "execute effect should be Draw, got {:?}",
            execute.effect
        );
        // Unless pay should be DynamicGeneric with SelfPower
        let unless_pay = def.unless_pay.as_ref().expect("should have unless_pay");
        assert_eq!(
            unless_pay.cost,
            UnlessCost::DynamicGeneric {
                quantity: QuantityExpr::Ref {
                    qty: QuantityRef::SelfPower
                }
            }
        );
        assert_eq!(unless_pay.payer, TargetFilter::TriggeringPlayer);
    }

    #[test]
    fn trigger_put_into_graveyard_from_battlefield_self() {
        // CR 700.4: "Is put into a graveyard from the battlefield" is a synonym for "dies."
        let def = parse_trigger_line(
            "When ~ is put into a graveyard from the battlefield, return ~ to its owner's hand.",
            "Rancor",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_put_into_graveyard_from_battlefield_another_creature() {
        // plural "are put into a graveyard from the battlefield"
        let def = parse_trigger_line(
            "Whenever a creature you control is put into a graveyard from the battlefield, you gain 1 life.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
    }

    #[test]
    fn trigger_blocks_self() {
        let def = parse_trigger_line(
            "Whenever Sustainer of the Realm blocks, it gains +0/+2 until end of turn.",
            "Sustainer of the Realm",
        );
        assert_eq!(def.mode, TriggerMode::Blocks);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_blocks_when_prefix() {
        let def = parse_trigger_line(
            "When Stoic Ephemera blocks, it deals 5 damage to each creature blocking or blocked by it.",
            "Stoic Ephemera",
        );
        assert_eq!(def.mode, TriggerMode::Blocks);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_blocks_a_creature() {
        let def = parse_trigger_line(
            "Whenever Wall of Frost blocks a creature, that creature doesn't untap during its controller's next untap step.",
            "Wall of Frost",
        );
        assert_eq!(def.mode, TriggerMode::Blocks);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_blocks_or_becomes_blocked() {
        // "blocks or becomes blocked" — parsed as Blocks (blocker side)
        let def = parse_trigger_line(
            "Whenever Karn, Silver Golem blocks or becomes blocked, it gets -4/+4 until end of turn.",
            "Karn, Silver Golem",
        );
        assert_eq!(def.mode, TriggerMode::Blocks);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_creature_you_control_blocks() {
        let def = parse_trigger_line(
            "Whenever a creature you control blocks, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::Blocks);
    }

    #[test]
    fn trigger_chaos_ensues_mode() {
        let def = parse_trigger_line("Whenever chaos ensues, draw a card.", "Plane");
        assert_eq!(def.mode, TriggerMode::ChaosEnsues);
    }

    #[test]
    fn trigger_set_in_motion_mode() {
        let def = parse_trigger_line("When you set this scheme in motion, draw a card.", "Scheme");
        assert_eq!(def.mode, TriggerMode::SetInMotion);
    }

    #[test]
    fn trigger_crank_contraption_mode() {
        let def = parse_trigger_line(
            "Whenever you crank this Contraption, create a token.",
            "Contraption",
        );
        assert_eq!(def.mode, TriggerMode::CrankContraption);
    }

    #[test]
    fn trigger_turn_face_up_mode() {
        let def = parse_trigger_line(
            "When this creature is turned face up, draw a card.",
            "Morphling",
        );
        assert_eq!(def.mode, TriggerMode::TurnFaceUp);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_commit_crime_mode() {
        let def = parse_trigger_line("Whenever you commit a crime, draw a card.", "At Knifepoint");
        assert_eq!(def.mode, TriggerMode::CommitCrime);
    }

    #[test]
    fn trigger_day_night_changes_mode() {
        let def = parse_trigger_line(
            "Whenever day becomes night or night becomes day, draw a card.",
            "Firmament Sage",
        );
        assert_eq!(def.mode, TriggerMode::DayTimeChanges);
    }

    #[test]
    fn trigger_end_of_combat_phase() {
        let def = parse_trigger_line(
            "At end of combat, sacrifice this creature.",
            "Ball Lightning",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::EndCombat));
    }

    #[test]
    fn trigger_becomes_target_mode() {
        let def = parse_trigger_line(
            "When this creature becomes the target of a spell or ability, sacrifice it.",
            "Frost Walker",
        );
        assert_eq!(def.mode, TriggerMode::BecomesTarget);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(def.valid_source, None); // spell OR ability — no source filter
    }

    #[test]
    fn trigger_becomes_target_of_spell_only() {
        let def = parse_trigger_line(
            "Whenever this creature becomes the target of a spell, this creature deals 2 damage to that spell's controller.",
            "Bonecrusher Giant",
        );
        assert_eq!(def.mode, TriggerMode::BecomesTarget);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(def.valid_source, Some(TargetFilter::StackSpell));
    }

    #[test]
    fn trigger_put_into_graveyard_from_anywhere() {
        let def = parse_trigger_line(
            "When this card is put into a graveyard from anywhere, draw a card.",
            "Dread",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(def.origin, None);
    }

    #[test]
    fn trigger_you_discard_a_card() {
        let def = parse_trigger_line(
            "Whenever you discard a card, draw a card.",
            "Bag of Holding",
        );
        assert_eq!(def.mode, TriggerMode::Discarded);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Card).controller(ControllerRef::You)
            ))
        );
    }

    #[test]
    fn trigger_opponent_discards_a_card() {
        let def = parse_trigger_line(
            "Whenever an opponent discards a card, draw a card.",
            "Geth's Grimoire",
        );
        assert_eq!(def.mode, TriggerMode::Discarded);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Card).controller(ControllerRef::Opponent)
            ))
        );
    }

    #[test]
    fn trigger_you_sacrifice_another_permanent() {
        let def = parse_trigger_line(
            "Whenever you sacrifice another permanent, draw a card.",
            "Furnace Celebration",
        );
        assert_eq!(def.mode, TriggerMode::Sacrificed);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Permanent)
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Another])
            ))
        );
    }

    #[test]
    fn trigger_player_cycles_a_card() {
        let def = parse_trigger_line(
            "Whenever a player cycles a card, draw a card.",
            "Astral Slide",
        );
        assert_eq!(def.mode, TriggerMode::Cycled);
    }

    #[test]
    fn trigger_spell_cast_or_copy_mode() {
        let def = parse_trigger_line(
            "Whenever you cast or copy an instant or sorcery spell, create a Treasure token.",
            "Storm-Kiln Artist",
        );
        assert_eq!(def.mode, TriggerMode::SpellCastOrCopy);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_unlock_door_mode() {
        let def = parse_trigger_line("When you unlock this door, draw a card.", "Door");
        assert_eq!(def.mode, TriggerMode::UnlockDoor);
    }

    #[test]
    fn trigger_mutates_mode() {
        let def = parse_trigger_line("Whenever this creature mutates, draw a card.", "Gemrazer");
        assert_eq!(def.mode, TriggerMode::Mutates);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_becomes_untapped_mode() {
        let def = parse_trigger_line(
            "Whenever this creature becomes untapped, draw a card.",
            "Arbiter of the Ideal",
        );
        assert_eq!(def.mode, TriggerMode::Untaps);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn trigger_self_or_another_ally_enters() {
        let def = parse_trigger_line(
            "Whenever this creature or another Ally you control enters, you gain 1 life.",
            "Hada Freeblade",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert!(matches!(def.valid_card, Some(TargetFilter::Or { .. })));
        assert_eq!(def.destination, Some(Zone::Battlefield));
    }

    #[test]
    fn trigger_another_human_you_control_enters() {
        let def = parse_trigger_line(
            "Whenever another Human you control enters, draw a card.",
            "Welcoming Vampire",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Human".to_string())
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Another])
            ))
        );
    }

    #[test]
    fn trigger_dragon_you_control_attacks() {
        let def = parse_trigger_line(
            "Whenever a Dragon you control attacks, create a Treasure token.",
            "Ganax, Astral Hunter",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Dragon".to_string())
                    .controller(ControllerRef::You)
            ))
        );
    }

    #[test]
    fn trigger_samurai_or_warrior_attacks_alone() {
        let def = parse_trigger_line(
            "Whenever a Samurai or Warrior you control attacks alone, draw a card.",
            "Raiyuu, Storm's Edge",
        );
        // Now that parse_type_phrase recognizes subtypes ("Samurai", "Warrior"),
        // the trigger parser correctly identifies this as an Attacks trigger.
        assert!(matches!(def.mode, TriggerMode::Attacks));
    }

    #[test]
    fn trigger_this_siege_enters_is_self_etb() {
        let def = parse_trigger_line("When this Siege enters, draw a card.", "Invasion");
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    // --- Phase trigger possessive qualifier tests ---

    #[test]
    fn phase_trigger_your_upkeep() {
        let def = parse_trigger_line("At the beginning of your upkeep, draw a card.", "Test Card");
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn phase_trigger_combat_on_your_turn() {
        let def = parse_trigger_line(
            "At the beginning of combat on your turn, target creature gets +1/+1 until end of turn.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::BeginCombat));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn phase_trigger_each_players_upkeep_no_constraint() {
        let def = parse_trigger_line(
            "At the beginning of each player's upkeep, that player draws a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn phase_trigger_each_opponents_upkeep() {
        let def = parse_trigger_line(
            "At the beginning of each opponent's upkeep, this creature deals 1 damage to that player.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::OnlyDuringOpponentsTurn)
        );
    }

    #[test]
    fn phase_trigger_each_combat_no_constraint() {
        let def = parse_trigger_line(
            "At the beginning of each combat, create a 1/1 white Soldier creature token.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::BeginCombat));
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_optional_sub_ability_not_optional() {
        // "you may" applies to the first sentence only; the sub-ability
        // should not inherit optional.
        let def = parse_trigger_line(
            "When this creature enters, you may draw a card. Create a 1/1 white Soldier creature token.",
            "Some Card",
        );
        assert!(def.optional);
        let execute = def.execute.as_ref().unwrap();
        assert!(execute.optional, "root ability should be optional");
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("should have sub_ability");
        assert!(!sub.optional, "sub-ability should NOT be optional");
    }

    #[test]
    fn trigger_you_may_mid_chain_not_trigger_optional() {
        // "you may" is in the second sentence — trigger-level optional is false,
        // but the second sentence's ability should have optional = true.
        let def = parse_trigger_line(
            "When this creature enters, draw a card. You may discard a card.",
            "Some Card",
        );
        assert!(!def.optional, "trigger-level optional should be false");
        let execute = def.execute.as_ref().unwrap();
        assert!(!execute.optional, "root ability should NOT be optional");
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("should have sub_ability");
        assert!(sub.optional, "second sentence ability should be optional");
    }

    // ── Work Item 1: Leaves-Graveyard Batch Triggers ──────────────

    #[test]
    fn trigger_one_or_more_creature_cards_leave_graveyard() {
        let def = parse_trigger_line(
            "Whenever one or more creature cards leave your graveyard, create a 1/1 green and black Insect creature token.",
            "Insidious Roots",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.origin, Some(Zone::Graveyard));
        assert!(def.batched);
        assert!(def.valid_card.is_some());
    }

    #[test]
    fn trigger_one_or_more_cards_leave_graveyard() {
        let def = parse_trigger_line(
            "Whenever one or more cards leave your graveyard, put a +1/+1 counter on this creature.",
            "Chalk Outline",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.origin, Some(Zone::Graveyard));
        assert!(def.batched);
        assert_eq!(def.valid_card, None); // no type filter — "cards"
    }

    #[test]
    fn trigger_one_or_more_cards_leave_graveyard_during_your_turn() {
        let def = parse_trigger_line(
            "Whenever one or more cards leave your graveyard during your turn, you gain 1 life.",
            "Soul Enervation",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.origin, Some(Zone::Graveyard));
        assert!(def.batched);
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn trigger_one_or_more_artifact_or_creature_cards_leave_graveyard() {
        let def = parse_trigger_line(
            "Whenever one or more artifact and/or creature cards leave your graveyard, put a +1/+1 counter on this creature.",
            "Attuned Hunter",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.origin, Some(Zone::Graveyard));
        assert!(def.batched);
        assert!(matches!(def.valid_card, Some(TargetFilter::Or { .. })));
    }

    // ── Work Item 2: Discard Batch Triggers ───────────────────────

    #[test]
    fn trigger_you_discard_one_or_more_cards() {
        let def = parse_trigger_line(
            "Whenever you discard one or more cards, this creature gets +1/+0 until end of turn.",
            "Magmakin Artillerist",
        );
        assert_eq!(def.mode, TriggerMode::DiscardedAll);
        assert!(def.batched);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_one_or_more_players_discard() {
        let def = parse_trigger_line(
            "Whenever one or more players discard one or more cards, put a +1/+1 counter on this creature.",
            "Waste Not",
        );
        assert_eq!(def.mode, TriggerMode::DiscardedAll);
        assert!(def.batched);
        assert_eq!(def.valid_target, None); // any player
    }

    // ── Work Item 3: Noncombat Damage to Opponent ─────────────────

    #[test]
    fn trigger_noncombat_damage_to_opponent() {
        let def = parse_trigger_line(
            "Whenever a source you control deals noncombat damage to an opponent, create a 1/1 red Elemental creature token.",
            "Virtue of Courage",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::NoncombatOnly);
        assert!(matches!(
            def.valid_source,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                ..
            }))
        ));
        assert!(matches!(
            def.valid_target,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::Opponent),
                ..
            }))
        ));
    }

    // ── Work Item 4: Transforms Into Self ─────────────────────────

    #[test]
    fn trigger_transforms_into_self() {
        let def = parse_trigger_line(
            "When this creature transforms into Trystan, Penitent Culler, you gain 3 life.",
            "Trystan, Penitent Culler",
        );
        assert_eq!(def.mode, TriggerMode::Transformed);
        assert_eq!(def.valid_source, Some(TargetFilter::SelfRef));
    }

    // ── Work Item 5: Tap Opponent's Creature ──────────────────────

    #[test]
    fn trigger_you_tap_opponent_creature() {
        let def = parse_trigger_line(
            "Whenever you tap an untapped creature an opponent controls, you gain 1 life.",
            "Hylda of the Icy Crown",
        );
        assert_eq!(def.mode, TriggerMode::Taps);
        assert!(matches!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::Opponent),
                ..
            }))
        ));
    }

    // ── Work Item 6: Expend Triggers ──────────────────────────────

    #[test]
    fn trigger_expend_4() {
        let def = parse_trigger_line(
            "Whenever you expend 4, put a +1/+1 counter on this creature.",
            "Roughshod Duo",
        );
        assert_eq!(def.mode, TriggerMode::ManaExpend);
        assert_eq!(def.expend_threshold, Some(4));
    }

    #[test]
    fn trigger_expend_8() {
        let def = parse_trigger_line("Whenever you expend 8, draw a card.", "Wandertale Mentor");
        assert_eq!(def.mode, TriggerMode::ManaExpend);
        assert_eq!(def.expend_threshold, Some(8));
    }

    #[test]
    fn trigger_plural_deal_combat_damage() {
        // CR 120.1: Plural "deal" for &-names after ~ normalization
        let def = parse_trigger_line(
            "Whenever Dark Leo & Shredder deal combat damage to a player, create a 1/1 black Ninja creature token.",
            "Dark Leo & Shredder",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    }

    #[test]
    fn trigger_singular_deals_combat_damage_regression() {
        // Ensure singular "deals" still works
        let def = parse_trigger_line(
            "Whenever Ninja of the Deep Hours deals combat damage to a player, you may draw a card.",
            "Ninja of the Deep Hours",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
    }

    #[test]
    fn trigger_one_or_more_ninja_or_rogue_combat_damage() {
        // CR 205.3m + CR 603.10c: Compound subtype in "one or more" batched damage trigger
        let result = try_parse_one_or_more_combat_damage_to_player(
            "whenever one or more ninja or rogue creatures you control deal combat damage to a player",
        );
        assert!(
            result.is_some(),
            "should parse one-or-more compound trigger"
        );
        let (mode, def) = result.unwrap();
        assert_eq!(mode, TriggerMode::DamageDoneOnceByController);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert!(
            matches!(&def.valid_source, Some(TargetFilter::Or { filters }) if filters.len() == 2)
        );
    }

    #[test]
    fn trigger_etb_from_hand_if_attacking() {
        // Thousand-Faced Shadow: "When this creature enters from your hand, if it's attacking, ..."
        let def = parse_trigger_line(
            "When this creature enters from your hand, if it's attacking, create a token that's a copy of another target attacking creature. The token enters tapped and attacking.",
            "Thousand-Faced Shadow",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(def.origin, Some(Zone::Hand));
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(def.condition, Some(TriggerCondition::SourceIsAttacking));
        // Effect should be CopyTokenOf
        assert!(def.execute.is_some());
        let exec = def.execute.as_ref().unwrap();
        assert!(matches!(*exec.effect, Effect::CopyTokenOf { .. }));
    }

    #[test]
    fn ninjutsu_variant_paid_sneak_condition() {
        // CR 702.49: "if its sneak cost was paid" → NinjutsuVariantPaid { variant: Sneak }
        let def = parse_trigger_line(
            "When this creature enters, if its sneak cost was paid, draw a card.",
            "Test Ninja",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::NinjutsuVariantPaid {
                variant: NinjutsuVariant::Sneak,
            })
        );
    }

    #[test]
    fn ninjutsu_variant_paid_ninjutsu_condition() {
        // CR 702.49: "if its ninjutsu cost was paid" → NinjutsuVariantPaid { variant: Ninjutsu }
        let def = parse_trigger_line(
            "When this creature enters, if its ninjutsu cost was paid, target opponent discards a card.",
            "Test Ninja",
        );
        assert_eq!(
            def.condition,
            Some(TriggerCondition::NinjutsuVariantPaid {
                variant: NinjutsuVariant::Ninjutsu,
            })
        );
    }

    // --- CR 115.9c: "that targets only [X]" trigger tests ---

    #[test]
    fn trigger_zada_targets_only_self() {
        let def = parse_trigger_line(
            "Whenever you cast an instant or sorcery spell that targets only Zada, copy that spell for each other creature you control.",
            "Zada, Hedron Grinder",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        // valid_card should be Or(Instant, Sorcery) with TargetsOnly { SelfRef } on each
        let valid_card = def.valid_card.expect("should have valid_card");
        if let TargetFilter::Or { filters } = &valid_card {
            assert_eq!(filters.len(), 2, "expected 2 branches for instant/sorcery");
            for f in filters {
                if let TargetFilter::Typed(tf) = f {
                    assert!(
                        tf.properties.iter().any(|p| matches!(p, FilterProp::TargetsOnly { filter } if **filter == TargetFilter::SelfRef)),
                        "expected TargetsOnly(SelfRef) in {tf:?}"
                    );
                } else {
                    panic!("expected Typed filter, got {f:?}");
                }
            }
        } else {
            panic!("expected Or filter, got {valid_card:?}");
        }
    }

    #[test]
    fn trigger_leyline_of_resonance_targets_only_single_creature_you_control() {
        let def = parse_trigger_line(
            "Whenever you cast an instant or sorcery spell that targets only a single creature you control, copy that spell.",
            "Leyline of Resonance",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        let valid_card = def.valid_card.expect("should have valid_card");
        if let TargetFilter::Or { filters } = &valid_card {
            assert_eq!(filters.len(), 2);
            for f in filters {
                if let TargetFilter::Typed(tf) = f {
                    assert!(
                        tf.properties
                            .iter()
                            .any(|p| matches!(p, FilterProp::TargetsOnly { .. })),
                        "expected TargetsOnly in {tf:?}"
                    );
                    assert!(
                        tf.properties
                            .iter()
                            .any(|p| matches!(p, FilterProp::HasSingleTarget)),
                        "expected HasSingleTarget in {tf:?}"
                    );
                } else {
                    panic!("expected Typed filter, got {f:?}");
                }
            }
        } else {
            panic!("expected Or filter, got {valid_card:?}");
        }
    }

    #[test]
    fn enters_tapped_and_attacking_patches_change_zone() {
        // CR 508.4: Shark Shredder — "put ... onto the battlefield under your control.
        // It enters tapped and attacking that player."
        let def = parse_trigger_line(
            "Whenever Shark Shredder deals combat damage to a player, put up to one target creature card from that player's graveyard onto the battlefield under your control. It enters tapped and attacking that player.",
            "Shark Shredder, Killer Clone",
        );
        assert_eq!(def.mode, TriggerMode::DamageDone);
        let exec = def.execute.as_ref().unwrap();
        // The primary effect should be ChangeZone with enter_tapped + enters_attacking.
        match &*exec.effect {
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                under_your_control: true,
                enter_tapped: true,
                enters_attacking: true,
                ..
            } => {} // expected
            other => panic!(
                "expected ChangeZone with enter_tapped + enters_attacking, got {:?}",
                other
            ),
        }
        // The sub_ability should NOT be Unimplemented.
        if let Some(sub) = &exec.sub_ability {
            assert!(
                !matches!(*sub.effect, Effect::Unimplemented { .. }),
                "sub_ability should not be Unimplemented, got {:?}",
                sub.effect,
            );
        }
    }

    #[test]
    fn enters_tapped_and_attacking_patches_token() {
        // CR 508.4: Stangg — "create ... token. It enters tapped and attacking."
        let def = parse_trigger_line(
            "Whenever Stangg attacks, create Stangg Twin, a legendary 3/4 red and green Human Warrior creature token. It enters tapped and attacking.",
            "Stangg, Echo Warrior",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        let exec = def.execute.as_ref().unwrap();
        match &*exec.effect {
            Effect::Token {
                tapped: true,
                enters_attacking: true,
                ..
            } => {} // expected
            other => panic!(
                "expected Token with tapped + enters_attacking, got {:?}",
                other
            ),
        }
    }

    // -----------------------------------------------------------------------
    // ChangesZone "put into graveyard" sub-pattern tests (Phase 35-01)
    // -----------------------------------------------------------------------

    #[test]
    fn trigger_put_into_graveyard_from_battlefield() {
        // CR 700.4: "is put into a graveyard from the battlefield" == "dies"
        let def = parse_trigger_line(
            "Whenever a creature is put into a graveyard from the battlefield, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert!(def.valid_card.is_some());
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_creature_card_put_into_graveyard_from_anywhere() {
        // "from anywhere" means no origin restriction (typed subject)
        let def = parse_trigger_line(
            "Whenever a creature card is put into a graveyard from anywhere, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, None);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert!(def.valid_card.is_some());
    }

    #[test]
    fn trigger_put_into_opponents_graveyard() {
        let def = parse_trigger_line(
            "Whenever a card is put into an opponent's graveyard from anywhere, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, None);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            ))
        );
    }

    // -----------------------------------------------------------------------
    // Phase trigger variant tests (35-02)
    // -----------------------------------------------------------------------

    #[test]
    fn trigger_end_of_combat_your_turn() {
        // CR 511.2: "At end of combat on your turn" restricts to controller's turn.
        let def = parse_trigger_line(
            "At end of combat on your turn, exile target creature you control, then return it to the battlefield under your control.",
            "Thassa, Deep-Dwelling",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::EndCombat));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn trigger_the_end_of_combat_your_turn() {
        // CR 511.2: Alternate phrasing "at the end of combat on your turn".
        let def = parse_trigger_line(
            "At the end of combat on your turn, put a +1/+1 counter on each creature that attacked this turn.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::EndCombat));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn trigger_end_of_combat_no_constraint() {
        // CR 511.2: Bare "at end of combat" with no turn qualifier has no constraint.
        let def = parse_trigger_line(
            "At end of combat, sacrifice this creature.",
            "Ball Lightning",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::EndCombat));
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_each_end_step() {
        // CR 513.1: "each end step" fires every turn with no controller constraint.
        let def = parse_trigger_line(
            "At the beginning of each end step, each player draws a card.",
            "Howling Mine",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_the_end_step() {
        // CR 513.1: "the end step" with no possessive — fires each turn.
        let def = parse_trigger_line(
            "At the beginning of the end step, sacrifice this creature.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_each_upkeep() {
        // CR 503.1a: "each upkeep" fires every turn with no controller constraint.
        let def = parse_trigger_line(
            "At the beginning of each upkeep, each player loses 1 life.",
            "Sulfuric Vortex",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::Upkeep));
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_phase_with_if_condition() {
        // Intervening-if condition is extracted by extract_if_condition upstream.
        let def = parse_trigger_line(
            "At the beginning of your end step, if you gained life this turn, draw a card.",
            "Dawn of Hope",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::End));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::GainedLife { minimum: 1 })
        );
    }

    #[test]
    fn trigger_put_into_your_graveyard_from_library() {
        let def = parse_trigger_line(
            "Whenever a creature card is put into your graveyard from your library, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Library));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            ))
        );
    }

    #[test]
    fn trigger_one_or_more_creature_cards_put_into_graveyard_from_library() {
        // CR 603.10c: "One or more" triggers fire once per batch
        let def = parse_trigger_line(
            "Whenever one or more creature cards are put into your graveyard from your library, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.origin, Some(Zone::Library));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert!(def.batched);
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            ))
        );
        // Subject filter should be creature type
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "Expected Creature in type_filters, got {:?}",
                tf.type_filters
            );
        } else {
            panic!("Expected Typed creature filter, got {:?}", def.valid_card);
        }
    }

    #[test]
    fn trigger_nontoken_creature_put_into_graveyard() {
        let def = parse_trigger_line(
            "Whenever a nontoken creature is put into your graveyard, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        // Should have Non(Subtype("Token")) in the type_filters
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert!(
                tf.type_filters.iter().any(|t| matches!(
                    t,
                    TypeFilter::Non(inner) if matches!(&**inner, TypeFilter::Subtype(s) if s == "Token")
                )),
                "Expected Non(Subtype(Token)) in type_filters, got {:?}",
                tf.type_filters
            );
        } else {
            panic!(
                "Expected Typed filter with Non(Token), got {:?}",
                def.valid_card
            );
        }
    }

    #[test]
    fn trigger_creature_with_power_4_or_greater_enters() {
        let def = parse_trigger_line(
            "Whenever a creature with power 4 or greater enters the battlefield under your control, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.destination, Some(Zone::Battlefield));
        // Should have PowerGE { value: 4 } in the filter props
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::PowerGE { value: 4 })),
                "Expected PowerGE(4) in properties, got {:?}",
                tf.properties
            );
        } else {
            panic!(
                "Expected Typed filter with PowerGE, got {:?}",
                def.valid_card
            );
        }
    }

    #[test]
    fn trigger_face_down_creature_dies() {
        let def = parse_trigger_line(
            "Whenever a face-down creature you control dies, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        // Should have FaceDown in the filter props
        if let Some(TargetFilter::Typed(tf)) = &def.valid_card {
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::FaceDown)),
                "Expected FaceDown in properties, got {:?}",
                tf.properties
            );
            assert_eq!(tf.controller, Some(ControllerRef::You));
        } else {
            panic!(
                "Expected Typed filter with FaceDown, got {:?}",
                def.valid_card
            );
        }
    }

    #[test]
    fn trigger_put_into_your_graveyard_no_origin() {
        // "is put into your graveyard" without "from" clause
        let def = parse_trigger_line(
            "Whenever a creature is put into your graveyard, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, None);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(
            def.valid_target,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            ))
        );
    }

    #[test]
    fn trigger_one_or_more_cards_put_into_graveyard_from_anywhere() {
        let def = parse_trigger_line(
            "Whenever one or more cards are put into your graveyard from anywhere, draw a card.",
            "Some Card",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZoneAll);
        assert_eq!(def.origin, None);
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert!(def.batched);
        // "cards" with no type restriction should have no valid_card filter
        assert_eq!(def.valid_card, None);
    }

    #[test]
    fn trigger_precombat_main_phase() {
        // CR 505.1: "precombat main phase" maps to PreCombatMain.
        let def = parse_trigger_line(
            "At the beginning of your precombat main phase, add one mana of any color.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::PreCombatMain));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn trigger_postcombat_main_phase() {
        // CR 505.1: "postcombat main phase" maps to PostCombatMain.
        let def = parse_trigger_line(
            "At the beginning of each player's postcombat main phase, that player may cast a spell.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::PostCombatMain));
        // "each player's" has no "your" or "opponent" → no constraint
        assert_eq!(def.constraint, None);
    }

    #[test]
    fn trigger_first_main_phase() {
        // CR 505.1: "first main phase" is an alias for precombat main phase.
        let def = parse_trigger_line(
            "At the beginning of your first main phase, add one mana of any color.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::PreCombatMain));
        assert_eq!(def.constraint, Some(TriggerConstraint::OnlyDuringYourTurn));
    }

    #[test]
    fn trigger_second_main_phase() {
        // CR 505.1: "second main phase" is an alias for postcombat main phase.
        let def = parse_trigger_line(
            "At the beginning of each player's second main phase, that player draws a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::Phase);
        assert_eq!(def.phase, Some(Phase::PostCombatMain));
        assert_eq!(def.constraint, None);
    }

    // --- Plan 03: Attacks trigger sub-patterns ---

    #[test]
    fn trigger_enchanted_player_attacked() {
        // CR 508.1a: "enchanted player is attacked" — AttachedTo as defending player.
        let def = parse_trigger_line(
            "Whenever enchanted player is attacked, create a 1/1 white Soldier creature token.",
            "Curse of the Forsaken",
        );
        assert_eq!(def.mode, TriggerMode::Attacks);
        assert_eq!(def.valid_target, Some(TargetFilter::AttachedTo));
        assert!(def.execute.is_some());
    }

    #[test]
    fn trigger_two_or_more_creatures_attack() {
        // CR 508.1a: "two or more" uses MinCoAttackers with minimum=1 (2-1).
        let def = parse_trigger_line(
            "Whenever two or more creatures you control attack a player, draw a card.",
            "Edric, Spymaster of Trest",
        );
        assert_eq!(def.mode, TriggerMode::YouAttack);
        assert_eq!(
            def.condition,
            Some(TriggerCondition::MinCoAttackers { minimum: 1 })
        );
        assert_eq!(def.valid_target, Some(TargetFilter::Player));
        assert!(def.execute.is_some());
    }

    // --- Plan 03: SpellCast trigger sub-patterns ---

    #[test]
    fn trigger_first_spell_opponents_turn() {
        // CR 601.2: "first spell during each opponent's turn"
        let def = parse_trigger_line(
            "Whenever you cast your first spell during each opponent's turn, draw a card.",
            "Faerie Mastermind",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(
            def.constraint,
            Some(TriggerConstraint::NthSpellThisTurn { n: 1, filter: None })
        );
        assert_eq!(def.condition, Some(TriggerCondition::DuringOpponentsTurn));
    }

    #[test]
    fn trigger_copy_spell() {
        // CR 707.10: "you copy a spell" maps to SpellCopy.
        let def = parse_trigger_line(
            "Whenever you copy a spell, put a +1/+1 counter on ~.",
            "Ivy, Gleeful Spellthief",
        );
        assert_eq!(def.mode, TriggerMode::SpellCopy);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        assert!(def.execute.is_some());
    }

    // --- Plan 03: DamageDone trigger sub-patterns ---

    #[test]
    fn trigger_dealt_damage_by_source_dies() {
        // CR 700.4 + CR 120.1: "a creature dealt damage by ~ this turn dies"
        let def = parse_trigger_line(
            "Whenever a creature dealt damage by Syr Konrad, the Grim this turn dies, each opponent loses 1 life.",
            "Syr Konrad, the Grim",
        );
        assert_eq!(def.mode, TriggerMode::ChangesZone);
        assert_eq!(def.origin, Some(Zone::Battlefield));
        assert_eq!(def.destination, Some(Zone::Graveyard));
        assert_eq!(
            def.condition,
            Some(TriggerCondition::DealtDamageBySourceThisTurn)
        );
    }

    #[test]
    fn trigger_you_dealt_damage() {
        // CR 120.1: "whenever you're dealt damage" — player damage received.
        let def = parse_trigger_line(
            "Whenever you're dealt damage, put that many charge counters on ~.",
            "Stuffy Doll",
        );
        assert_eq!(def.mode, TriggerMode::DamageReceived);
        assert_eq!(def.damage_kind, DamageKindFilter::Any);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_you_dealt_combat_damage() {
        // CR 120.1a: "whenever you're dealt combat damage" — combat-only variant.
        let def = parse_trigger_line(
            "Whenever you're dealt combat damage, draw a card.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::DamageReceived);
        assert_eq!(def.damage_kind, DamageKindFilter::CombatOnly);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
    }

    #[test]
    fn trigger_opponent_dealt_noncombat_damage() {
        // CR 120.1b: "whenever an opponent is dealt noncombat damage"
        let def = parse_trigger_line(
            "Whenever an opponent is dealt noncombat damage, you gain 1 life.",
            "Test Card",
        );
        assert_eq!(def.mode, TriggerMode::DamageReceived);
        assert_eq!(def.damage_kind, DamageKindFilter::NoncombatOnly);
    }

    // --- Plan 03: CounterRemoved trigger sub-patterns ---

    #[test]
    fn trigger_time_counter_removed_exile() {
        // CR 121.6: "a time counter is removed from ~ while it's exiled"
        let def = parse_trigger_line(
            "Whenever a time counter is removed from ~ while it's exiled, you may cast a copy of ~ without paying its mana cost.",
            "Rift Bolt",
        );
        assert_eq!(def.mode, TriggerMode::CounterRemoved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(def.trigger_zones, vec![Zone::Exile]);
    }

    #[test]
    fn trigger_counter_removed_no_zone_constraint() {
        // CR 121.6: "a time counter is removed from ~" without zone constraint.
        let def = parse_trigger_line(
            "Whenever a time counter is removed from ~, deal 1 damage to any target.",
            "Test Suspend Card",
        );
        assert_eq!(def.mode, TriggerMode::CounterRemoved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        // No zone constraint — fires from default zones
        assert_eq!(def.trigger_zones, vec![Zone::Battlefield]);
    }

    // -----------------------------------------------------------------------
    // CR 608.2k: Trigger pronoun resolution — "it"/"its" context-dependent
    // -----------------------------------------------------------------------

    #[test]
    fn trigger_it_resolves_to_triggering_source_for_non_self_subject() {
        // "it" refers to the entering creature, not the enchantment
        let def = parse_trigger_line(
            "Whenever a creature you control enters, put a +1/+1 counter on it",
            "Test Enchantment",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        match &*exec.effect {
            Effect::PutCounter { target, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::TriggeringSource,
                    "non-self trigger 'it' should resolve to TriggeringSource"
                );
            }
            other => panic!("Expected PutCounter, got {:?}", other),
        }
    }

    #[test]
    fn trigger_it_stays_self_ref_for_self_subject() {
        // "it" refers to ~ (the card itself entering)
        let def = parse_trigger_line(
            "When Test Card enters, put a +1/+1 counter on it",
            "Test Card",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        match &*exec.effect {
            Effect::PutCounter { target, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::SelfRef,
                    "self-trigger 'it' should stay SelfRef"
                );
            }
            other => panic!("Expected PutCounter, got {:?}", other),
        }
    }

    #[test]
    fn trigger_tilde_stays_self_ref_with_non_self_subject() {
        // "~" always refers to the source permanent, even in non-self trigger
        let def = parse_trigger_line(
            "Whenever a creature you control enters, sacrifice ~",
            "Test Enchantment",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        match &*exec.effect {
            Effect::Sacrifice { target } => {
                assert_eq!(*target, TargetFilter::SelfRef, "~ should always be SelfRef");
            }
            other => panic!("Expected Sacrifice, got {:?}", other),
        }
    }

    #[test]
    fn trigger_otherwise_branch_preserves_context() {
        // Tribute to the World Tree pattern: else_ability "it" = triggering creature
        let def = parse_trigger_line(
            "Whenever a creature you control enters, draw a card if its power is 3 or greater. Otherwise, put two +1/+1 counters on it.",
            "Tribute to the World Tree",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        let else_ab = exec
            .else_ability
            .as_ref()
            .expect("should have else_ability");
        match &*else_ab.effect {
            Effect::PutCounter { target, count, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::TriggeringSource,
                    "else_ability 'it' should be TriggeringSource"
                );
                assert_eq!(*count, 2);
            }
            other => panic!("Expected PutCounter in else_ability, got {:?}", other),
        }
    }

    #[test]
    fn trigger_subject_predicate_it_gains() {
        // "it gains haste" — subject-predicate with "it" as subject.
        // The subject "it" resolves to TriggeringSource and lands in the
        // static_abilities[0].affected field (not the top-level `target`).
        let def = parse_trigger_line(
            "Whenever a creature you control enters, it gains haste until end of turn",
            "Test Enchantment",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        match &*exec.effect {
            Effect::GenericEffect {
                static_abilities, ..
            } => {
                assert_eq!(
                    static_abilities[0].affected,
                    Some(TargetFilter::TriggeringSource),
                    "subject-predicate 'it' should produce TriggeringSource in affected"
                );
            }
            other => panic!("Expected GenericEffect, got {:?}", other),
        }
    }

    #[test]
    fn trigger_equipped_creature_it_resolves_to_triggering_source() {
        // "it" = equipped creature (AttachedTo subject → TriggeringSource)
        let def = parse_trigger_line(
            "Whenever equipped creature attacks, put a +1/+1 counter on it",
            "Test Equipment",
        );
        let exec = def.execute.as_ref().expect("should have execute");
        match &*exec.effect {
            Effect::PutCounter { target, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::TriggeringSource,
                    "equipped creature 'it' should be TriggeringSource"
                );
            }
            other => panic!("Expected PutCounter, got {:?}", other),
        }
    }

    // --- CR 115.9b: "that targets" trigger integration tests ---

    #[test]
    fn trigger_heroic_that_targets_self() {
        let def = parse_trigger_line(
            "Heroic — Whenever you cast a spell that targets this creature, put a +1/+1 counter on each creature you control.",
            "Phalanx Leader",
        );
        assert_eq!(def.mode, TriggerMode::SpellCast);
        assert_eq!(def.valid_target, Some(TargetFilter::Controller));
        // valid_card should have Targets { SelfRef } property
        let valid_card = def.valid_card.expect("should have valid_card");
        if let TargetFilter::Typed(tf) = &valid_card {
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::Targets { filter } if **filter == TargetFilter::SelfRef)),
                "expected Targets {{ SelfRef }} in properties: {:?}",
                tf.properties
            );
        } else {
            panic!("expected Typed filter, got {valid_card:?}");
        }
    }

    #[test]
    fn extract_if_you_have_n_or_more_life() {
        let (cleaned, cond) = extract_if_condition("draw a card if you have 40 or more life");
        assert!(
            matches!(cond, Some(TriggerCondition::LifeTotalGE { minimum: 40 })),
            "Expected LifeTotalGE {{ minimum: 40 }}, got: {cond:?}"
        );
        assert_eq!(cleaned.trim(), "draw a card");
    }

    #[test]
    fn extract_if_you_have_n_or_more_life_win() {
        let (cleaned, cond) = extract_if_condition("you win the game if you have 40 or more life");
        assert!(
            matches!(cond, Some(TriggerCondition::LifeTotalGE { .. })),
            "Expected LifeTotalGE, got: {cond:?}"
        );
        assert_eq!(cleaned.trim(), "you win the game");
    }

    #[test]
    fn extract_if_gained_life_regression() {
        // Existing pattern must still work
        let (_, cond) = extract_if_condition("draw a card if you've gained life this turn");
        assert!(
            matches!(cond, Some(TriggerCondition::GainedLife { minimum: 1 })),
            "Expected GainedLife, got: {cond:?}"
        );
    }
}
