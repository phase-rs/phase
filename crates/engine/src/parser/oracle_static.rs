use std::borrow::Cow;
use std::str::FromStr;

use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::parse_effect_chain;
use super::oracle_target::{parse_counter_suffix, parse_type_phrase};
use super::oracle_util::{parse_number, strip_reminder_text};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, AggregateFunction, ChosenSubtypeKind, Comparator,
    ContinuousModification, ControllerRef, CountScope, FilterProp, ObjectProperty, PlayerFilter,
    QuantityExpr, QuantityRef, StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
    TypedFilter, ZoneRef,
};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::statics::StaticMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleStaticPredicate {
    CantUntap,
    MustAttack,
    BlockOnlyCreaturesWithFlying,
    Shroud,
    MayLookAtTopOfLibrary,
    LoseAllAbilities,
    NoMaximumHandSize,
    MayPlayAdditionalLand,
}

/// Parse a static/continuous ability line into a StaticDefinition.
/// Handles: "Enchanted creature gets +N/+M", "has {keyword}",
/// "Creatures you control get +N/+M", etc.
pub fn parse_static_line(text: &str) -> Option<StaticDefinition> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();

    if lower.starts_with("you may choose not to untap ")
        && lower.contains(" during your untap step")
    {
        return Some(
            StaticDefinition::new(StaticMode::Other("MayChooseNotToUntap".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "Enchanted creature gets +N/+M" or "has {keyword}" ---
    if lower.starts_with("enchanted creature ") {
        if let Some(def) = parse_continuous_gets_has(
            &text[19..],
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])),
            &text,
        ) {
            return Some(def);
        }
    }

    // --- "Enchanted permanent gets/has ..." ---
    if lower.starts_with("enchanted permanent ") {
        if let Some(def) = parse_continuous_gets_has(
            &text[20..],
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy])),
            &text,
        ) {
            return Some(def);
        }
    }

    if lower.starts_with("enchanted land ") {
        if let Some(def) = parse_continuous_gets_has(
            &text[15..],
            TargetFilter::Typed(TypedFilter::land().properties(vec![FilterProp::EnchantedBy])),
            &text,
        ) {
            return Some(def);
        }
    }

    // --- "Equipped creature gets +N/+M" ---
    if lower.starts_with("equipped creature ") {
        if let Some(def) = parse_continuous_gets_has(
            &text[18..],
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy])),
            &text,
        ) {
            return Some(def);
        }
    }

    // --- "All creatures get/have ..." ---
    if lower.starts_with("all creatures ") {
        if let Some(def) = parse_continuous_gets_has(
            &text[14..],
            TargetFilter::Typed(TypedFilter::creature()),
            &text,
        ) {
            return Some(def);
        }
    }

    // --- "Each creature you control [with condition] assigns combat damage equal to its toughness" ---
    // CR 510.1c: Doran-class effects that cause creatures to use toughness for combat damage.
    if let Some(def) = parse_assigns_damage_from_toughness(&lower, &text) {
        return Some(def);
    }

    // --- "Other [Subtype] creatures you control get/have..." ---
    // e.g. "Other Zombies you control get +1/+1"
    if let Some(rest) = lower.strip_prefix("other ") {
        if let Some(result) = parse_typed_you_control(&text[6..], rest, true) {
            return Some(result);
        }
    }

    // --- "[Subtype] creatures you control get/have..." ---
    // e.g. "Elf creatures you control get +1/+1"
    if let Some(result) = parse_typed_you_control(&text, &lower, false) {
        return Some(result);
    }

    // --- "Creatures you control [with counter condition] get/have ..." ---
    if lower.starts_with("creatures you control ") {
        let after_prefix = &text[22..];
        let (filter, predicate_text) =
            if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                (
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![prop]),
                    ),
                    rest,
                )
            } else {
                (
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                    after_prefix,
                )
            };
        if let Some(def) = parse_continuous_gets_has(predicate_text, filter, &text) {
            return Some(def);
        }
    }

    // --- "Other creatures you control [with counter condition] get/have ..." ---
    if lower.starts_with("other creatures you control ") {
        let after_prefix = &text[28..];
        let (filter, predicate_text) =
            if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                (
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![prop]),
                    ),
                    rest,
                )
            } else {
                (
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                    after_prefix,
                )
            };
        if let Some(def) = parse_continuous_gets_has(predicate_text, filter, &text) {
            return Some(def);
        }
    }

    if let Some(def) = parse_subject_continuous_static(&text) {
        return Some(def);
    }

    // --- "Lands you control have '[type]'" ---
    if lower.starts_with("lands you control have ") {
        let rest = text[23..]
            .trim()
            .trim_end_matches('.')
            .trim_matches(|c: char| c == '\'' || c == '"');
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::land().controller(ControllerRef::You),
                ))
                .modifications(vec![ContinuousModification::AddSubtype {
                    subtype: rest.to_string(),
                }])
                .description(text.to_string()),
        );
    }

    // --- "During your turn, as long as ~ has [counters], [pronoun]'s a [P/T] [types] and has [keyword]" ---
    // Compound condition: DuringYourTurn + HasCounters → animation pattern (Kaito, Gideon, etc.)
    if let Some(def) = parse_compound_turn_counter_animation(&lower, &text) {
        return Some(def);
    }

    // --- "During your turn, [subject] has/gets ..." ---
    if let Some(rest) = lower.strip_prefix("during your turn, ") {
        let original_rest = &text["during your turn, ".len()..];
        if let Some(subject_end) = find_continuous_predicate_start(rest) {
            let subject = original_rest[..subject_end].trim();
            let predicate = original_rest[subject_end + 1..].trim();
            if let Some(affected) = parse_continuous_subject_filter(subject) {
                let modifications = parse_continuous_modifications(predicate);
                if !modifications.is_empty() {
                    return Some(
                        StaticDefinition::continuous()
                            .affected(affected)
                            .modifications(modifications)
                            .condition(StaticCondition::DuringYourTurn)
                            .description(text.to_string()),
                    );
                }
            }
        }
    }

    if let Some(def) = parse_subject_rule_static(&text) {
        return Some(def);
    }

    // --- "~ is the chosen type in addition to its other types" ---
    // Distinguish creature type (Metallic Mimic) vs basic land type (Multiversal Passage)
    if lower.contains("is the chosen type") {
        let kind = if lower.starts_with("this creature") || lower.contains("creature is the chosen")
        {
            ChosenSubtypeKind::CreatureType
        } else {
            ChosenSubtypeKind::BasicLandType
        };
        let modification = ContinuousModification::AddChosenSubtype { kind };
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![modification])
                .description(text.to_string()),
        );
    }

    // --- CDA: "~'s power is equal to the number of card types among cards in all graveyards
    //     and its toughness is equal to that number plus 1" (Tarmogoyf) ---
    if let Some(def) = parse_cda_pt_equality(&lower, &text) {
        return Some(def);
    }

    if let Some(def) = parse_conditional_static(&text) {
        return Some(def);
    }

    // --- "~ has [keyword] as long as ..." (must be before generic self-ref "has") ---
    if let Some(has_pos) = lower.find(" has ") {
        if let Some(cond_pos) = lower.find(" as long as ") {
            if has_pos < cond_pos {
                let keyword_text = lower[has_pos + 5..cond_pos].trim();
                let condition_text = text[cond_pos + 12..].trim().trim_end_matches('.');
                let mut modifications = Vec::new();
                if let Some(kw) = map_keyword(keyword_text) {
                    modifications.push(ContinuousModification::AddKeyword { keyword: kw });
                }
                let condition = parse_static_condition(condition_text).unwrap_or(
                    StaticCondition::Unrecognized {
                        text: condition_text.to_string(),
                    },
                );
                return Some(
                    StaticDefinition::continuous()
                        .affected(TargetFilter::SelfRef)
                        .modifications(modifications)
                        .condition(condition)
                        .description(text.to_string()),
                );
            }
        }
    }

    // --- "~ has/gets ..." (self-referential) ---
    // Match lines like "CARDNAME has deathtouch" or "CARDNAME gets +1/+1"
    if let Some(pos) = lower
        .find(" has ")
        .or_else(|| lower.find(" gets "))
        .or_else(|| lower.find(" get "))
    {
        let verb_len = if lower[pos..].starts_with(" has ") {
            5
        } else if lower[pos..].starts_with(" gets ") {
            6
        } else {
            5 // " get "
        };
        let subject = &lower[..pos];
        // Only match if the subject doesn't look like a known prefix we handle elsewhere
        if !subject.contains("creature")
            && !subject.contains("permanent")
            && !subject.contains("land")
            && !subject.starts_with("all ")
            && !subject.starts_with("other ")
        {
            let after = &text[pos + verb_len..];
            return parse_continuous_gets_has(
                &format!(
                    "{}{}",
                    if lower[pos..].starts_with(" has ") {
                        "has "
                    } else {
                        "gets "
                    },
                    after
                ),
                TargetFilter::SelfRef,
                &text,
            );
        }
    }

    // --- "~ isn't a [type]" (type removal) ---
    // e.g. "Erebos isn't a creature" from god-of-the-dead conditional
    if let Some(type_rest) = lower.split("isn't a ").nth(1) {
        use crate::types::card_type::CoreType;
        let type_name = type_rest.trim().trim_end_matches('.');
        let core_type = match type_name {
            "creature" => Some(CoreType::Creature),
            "artifact" => Some(CoreType::Artifact),
            "enchantment" => Some(CoreType::Enchantment),
            "land" => Some(CoreType::Land),
            "planeswalker" => Some(CoreType::Planeswalker),
            _ => None,
        };
        if let Some(ct) = core_type {
            return Some(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::RemoveType { core_type: ct }])
                    .description(text.to_string()),
            );
        }
    }

    // --- "~ can't be blocked" ---
    if lower.contains("can't be blocked") {
        return Some(
            StaticDefinition::new(StaticMode::Other("CantBeBlocked".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't block" ---
    if lower.contains("can't block") && !lower.contains("can't be blocked") {
        return Some(
            StaticDefinition::new(StaticMode::CantBlock)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't attack" ---
    if lower.contains("can't attack") {
        return Some(
            StaticDefinition::new(StaticMode::CantAttack)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't be countered" ---
    if lower.contains("can't be countered") {
        return Some(
            StaticDefinition::new(StaticMode::CantBeCast)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't be the target" or "~ can't be targeted" ---
    if lower.contains("can't be the target") || lower.contains("can't be targeted") {
        return Some(
            StaticDefinition::new(StaticMode::CantBeTargeted)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't be sacrificed" ---
    if lower.contains("can't be sacrificed") {
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ doesn't untap during your untap step" ---
    if lower.contains("doesn't untap during") || lower.contains("doesn\u{2019}t untap during") {
        return Some(
            StaticDefinition::new(StaticMode::Other("CantUntap".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "You may look at the top card of your library any time." ---
    if lower.starts_with("you may look at the top card of your library") {
        return Some(
            StaticDefinition::new(StaticMode::Other("MayLookAtTopOfLibrary".to_string()))
                .affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                ))
                .description(text.to_string()),
        );
    }

    // NOTE: "enters with N counters" patterns are now handled by oracle_replacement.rs
    // as proper Moved replacement effects (paralleling the "enters tapped" pattern).

    // --- "Spells you cast cost {N} less" ---
    if lower.contains("cost") && lower.contains("less") && lower.contains("spell") {
        return Some(
            StaticDefinition::new(StaticMode::ReduceCost)
                .affected(TargetFilter::Typed(
                    TypedFilter::card().controller(ControllerRef::You),
                ))
                .description(text.to_string()),
        );
    }

    // --- "Spells your opponents cast cost {N} more" ---
    if lower.contains("cost")
        && lower.contains("more")
        && lower.contains("spell")
        && lower.contains("opponent")
    {
        return Some(
            StaticDefinition::new(StaticMode::RaiseCost)
                .affected(TargetFilter::Typed(
                    TypedFilter::card().controller(ControllerRef::Opponent),
                ))
                .description(text.to_string()),
        );
    }

    // --- "must be blocked if able" (CR 509.1b) ---
    if lower.contains("must be blocked") {
        return Some(
            StaticDefinition::new(StaticMode::Other("MustBeBlocked".into()))
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "can't gain life" (CR 119.7) ---
    if lower.contains("can't gain life") {
        let affected = if lower.contains("your opponents") || lower.starts_with("opponents") {
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        } else if lower.starts_with("you ") || lower.contains("you can't") {
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You))
        } else {
            // "Players can't gain life" — affects all
            TargetFilter::Typed(TypedFilter::default())
        };
        return Some(
            StaticDefinition::new(StaticMode::CantGainLife)
                .affected(affected)
                .description(text.to_string()),
        );
    }

    // --- "as though it/they had flash" (CR 702.8d) ---
    if lower.contains("as though it had flash") || lower.contains("as though they had flash") {
        return Some(
            StaticDefinition::new(StaticMode::CastWithFlash).description(text.to_string()),
        );
    }

    // --- "can block an additional creature" / "can block any number" (CR 509.1b) ---
    if lower.contains("can block any number") {
        return Some(
            StaticDefinition::new(StaticMode::ExtraBlockers { count: None })
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }
    if lower.contains("can block an additional") {
        return Some(
            StaticDefinition::new(StaticMode::ExtraBlockers { count: Some(1) })
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "play an additional land" / "play two additional lands" ---
    if lower.contains("play an additional land") || lower.contains("play two additional lands") {
        return Some(
            StaticDefinition::new(StaticMode::Other("AdditionalLandDrop".into()))
                .description(text.to_string()),
        );
    }

    // --- "As long as ..." (generic conditional static, no comma separator) ---
    if lower.starts_with("as long as ") {
        let condition_text = text
            .strip_prefix("As long as ")
            .unwrap_or(&text)
            .trim_end_matches('.');
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .condition(StaticCondition::Unrecognized {
                    text: condition_text.to_string(),
                })
                .description(text.to_string()),
        );
    }

    // CR 603.9: Trigger doubling — "triggers an additional time"
    // Panharmonicon: "If a permanent entering the battlefield causes a triggered ability
    //   of a permanent you control to trigger, that ability triggers an additional time."
    // Roaming Throne: "If a triggered ability of another creature you control of the chosen
    //   type triggers, it triggers an additional time."
    if lower.contains("triggers an additional time") {
        return Some(
            StaticDefinition::new(StaticMode::Panharmonicon).description(text.to_string()),
        );
    }

    None
}

/// Try to parse "[Subtype] creatures you control get/have ..." patterns.
/// `text` is the original-case text starting at the subtype word.
/// `lower` is the lowercased version of `text`.
/// `is_other` indicates whether this was preceded by "Other ".
fn parse_typed_you_control(text: &str, lower: &str, is_other: bool) -> Option<StaticDefinition> {
    // Try "X creatures you control get/have" first
    if let Some(creatures_pos) = lower.find(" creatures you control ") {
        let descriptor = text[..creatures_pos].trim();
        if !descriptor.is_empty() {
            let after_prefix = &text[creatures_pos + 23..];
            let full_subject = text[..creatures_pos + " creatures you control".len()].trim();
            let typed_filter =
                if let Some(filter) = parse_modified_creature_subject_filter(full_subject) {
                    filter
                } else if let Some(color) = parse_named_color(descriptor) {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::HasColor {
                                color: color.to_string(),
                            }]),
                    )
                } else if is_capitalized_words(descriptor) {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .subtype(descriptor.to_string())
                            .controller(ControllerRef::You),
                    )
                } else {
                    return None;
                };
            // CR 613.7: Check for "with [counter] on it/them" condition between
            // "you control" and the predicate (e.g., "Elf creatures you control
            // with a +1/+1 counter on it has trample").
            let (typed_filter, after_prefix) =
                if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                    (add_property(typed_filter, prop), rest)
                } else {
                    (typed_filter, after_prefix)
                };
            let typed_filter = if is_other {
                add_another_filter(typed_filter)
            } else {
                typed_filter
            };
            return parse_continuous_gets_has(after_prefix, typed_filter, text);
        }
    }

    // Try "Xs you control get/have" (e.g. "Zombies you control get +1/+1")
    if let Some(yc_pos) = lower.find(" you control ") {
        let descriptor = text[..yc_pos].trim();
        if !descriptor.is_empty() {
            let after_prefix = &text[yc_pos + 13..];
            let full_subject = text[..yc_pos + " you control".len()].trim();
            let typed_filter =
                if let Some(filter) = parse_modified_creature_subject_filter(full_subject) {
                    filter
                } else if let Some(color) = parse_named_color(descriptor) {
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::HasColor {
                                color: color.to_string(),
                            }]),
                    )
                } else if is_capitalized_words(descriptor) {
                    // Strip trailing 's' for the subtype name (Zombies -> Zombie)
                    let subtype_name = descriptor.trim_end_matches('s').to_string();
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .subtype(subtype_name)
                            .controller(ControllerRef::You),
                    )
                } else {
                    return None;
                };
            // CR 613.7: Check for "with [counter] on it/them" condition
            let (typed_filter, after_prefix) =
                if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                    (add_property(typed_filter, prop), rest)
                } else {
                    (typed_filter, after_prefix)
                };
            let typed_filter = if is_other {
                add_another_filter(typed_filter)
            } else {
                typed_filter
            };
            return parse_continuous_gets_has(after_prefix, typed_filter, text);
        }
    }

    None
}

/// CR 510.1c: Parse "each creature you control [with condition] assigns combat damage
/// equal to its toughness rather than its power" patterns.
///
/// Supports three Oracle patterns:
/// - "each creature you control assigns combat damage equal to its toughness..."
/// - "each creature you control with defender assigns combat damage equal to its toughness..."
/// - "each creature you control with toughness greater than its power assigns combat damage..."
fn parse_assigns_damage_from_toughness(lower: &str, text: &str) -> Option<StaticDefinition> {
    let rest = lower.strip_prefix("each creature you control ")?;

    let suffix = "assigns combat damage equal to its toughness rather than its power";
    let suffix_alt = "assign combat damage equal to their toughness rather than their power";

    let (condition_text, _) = if let Some(pos) = rest.find(suffix) {
        (&rest[..pos], &rest[pos + suffix.len()..])
    } else if let Some(pos) = rest.find(suffix_alt) {
        (&rest[..pos], &rest[pos + suffix_alt.len()..])
    } else {
        return None;
    };

    let condition_text = condition_text.trim();

    let mut filter = TypedFilter::creature().controller(ControllerRef::You);

    if !condition_text.is_empty() {
        // Parse "with [condition]" clause
        let with_clause = condition_text.strip_prefix("with ")?;
        let with_clause = with_clause.trim();

        if with_clause == "toughness greater than its power" {
            filter = filter.properties(vec![FilterProp::ToughnessGTPower]);
        } else {
            // Treat as keyword condition: "with defender", "with flying", etc.
            // Validate it parses as a keyword, then store the lowercase string.
            let _keyword: Keyword = with_clause.parse().ok()?;
            filter = filter.properties(vec![FilterProp::WithKeyword {
                value: with_clause.to_string(),
            }]);
        }
    }

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(filter))
            .modifications(vec![ContinuousModification::AssignDamageFromToughness])
            .description(text.to_string()),
    )
}

fn parse_subject_rule_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let (affected, predicate_text) = strip_rule_static_subject(text, &lower)?;
    let predicate = parse_rule_static_predicate(predicate_text)?;
    Some(lower_rule_static(predicate, affected, text))
}

fn parse_subject_continuous_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();

    let subject_end = find_continuous_predicate_start(&lower)?;
    let subject = text[..subject_end].trim();
    let predicate = text[subject_end + 1..].trim();
    if parse_rule_static_predicate(predicate).is_some() {
        return None;
    }
    let affected = parse_continuous_subject_filter(subject)?;
    let modifications = parse_continuous_modifications(predicate);
    if !modifications.is_empty() {
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(modifications)
                .description(text.to_string()),
        );
    }

    None
}

/// Parse compound condition + animation pattern:
/// "During your turn, as long as ~ has one or more [counter] counters on [pronoun],
///  [pronoun]'s a [P/T] [types] and has [keyword]"
///
/// Produces `StaticCondition::And { DuringYourTurn, HasCounters { .. } }` with
/// `ContinuousModification` list for type/subtype/P-T/keyword changes.
fn parse_compound_turn_counter_animation(lower: &str, text: &str) -> Option<StaticDefinition> {
    // Strip "during your turn, " prefix
    let rest = lower.strip_prefix("during your turn, ")?;

    // Strip "as long as " prefix from the remainder
    let rest = rest.strip_prefix("as long as ")?;

    // Parse "~ has one or more [type] counters on [pronoun], "
    let rest = rest.strip_prefix("~ has ")?;

    // Parse the counter count requirement: "one or more" / "N or more" / "a"
    let (minimum, rest) = parse_counter_minimum(rest)?;

    // Parse "[type] counters on [pronoun], "
    let rest = rest.trim_start();
    let counters_pos = rest.find(" counter")?;
    let counter_type = rest[..counters_pos].trim().to_string();

    // Skip past "counters on [pronoun], " to get the modification text
    let rest = &rest[counters_pos..];
    let comma_pos = rest.find(", ")?;
    let modification_text = rest[comma_pos + 2..].trim();

    let modifications = parse_animation_modifications(modification_text.trim_end_matches('.'));
    if modifications.is_empty() {
        return None;
    }

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .condition(StaticCondition::And {
                conditions: vec![
                    StaticCondition::DuringYourTurn,
                    StaticCondition::HasCounters {
                        counter_type,
                        minimum,
                    },
                ],
            })
            .modifications(modifications)
            .description(text.to_string()),
    )
}

/// Parse "one or more" / "N or more" / "a" into a counter minimum count.
/// Returns (minimum, remaining text).
fn parse_counter_minimum(text: &str) -> Option<(u32, &str)> {
    if let Some(rest) = text.strip_prefix("one or more ") {
        return Some((1, rest));
    }
    if let Some(rest) = text.strip_prefix("a ") {
        return Some((1, rest));
    }
    // "N or more" pattern
    if let Some((n, rest)) = parse_number(text) {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix("or more ") {
            return Some((n, rest));
        }
    }
    None
}

/// Parse "[pronoun]'s a [P/T] [types] and has [keyword]" into modifications.
///
/// Handles patterns like:
/// - "he's a 3/4 ninja creature and has hexproof"
/// - "it's a 3/4 ninja creature with hexproof"
fn parse_animation_modifications(text: &str) -> Vec<ContinuousModification> {
    let lower = text.to_lowercase();
    let mut modifications = Vec::new();

    // Strip pronoun prefix: "he's a", "she's a", "it's a", "~'s a"
    let body = lower
        .strip_prefix("he's a ")
        .or_else(|| lower.strip_prefix("she's a "))
        .or_else(|| lower.strip_prefix("it's a "))
        .or_else(|| lower.strip_prefix("~'s a "));

    let body = match body {
        Some(b) => b.trim(),
        None => return modifications,
    };

    // Split on " and has " or " with " to separate type/PT from keywords
    let (type_pt_part, keyword_part) = if let Some(pos) = body.find(" and has ") {
        (&body[..pos], Some(&body[pos + 9..]))
    } else if let Some(pos) = body.find(" with ") {
        (&body[..pos], Some(&body[pos + 6..]))
    } else {
        (body, None)
    };

    // Parse P/T from the beginning: "3/4 ninja creature"
    let remaining = if let Some((p, t)) = parse_pt_mod(type_pt_part) {
        modifications.push(ContinuousModification::SetPower { value: p });
        modifications.push(ContinuousModification::SetToughness { value: t });
        // Skip past the P/T value
        let slash = type_pt_part.find('/').unwrap();
        let rest = &type_pt_part[slash + 1..];
        let pt_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        rest[pt_end..].trim()
    } else {
        type_pt_part
    };

    // Parse types and subtypes from remaining: "ninja creature", "human ninja creature"
    for word in remaining.split_whitespace() {
        let word = word.trim_end_matches('.').trim_end_matches(',');
        if word.is_empty() {
            continue;
        }
        use std::str::FromStr;
        let capitalized = format!("{}{}", word[..1].to_uppercase(), &word[1..]);
        if let Ok(core_type) = crate::types::card_type::CoreType::from_str(&capitalized) {
            modifications.push(ContinuousModification::AddType { core_type });
        } else {
            modifications.push(ContinuousModification::AddSubtype {
                subtype: capitalized,
            });
        }
    }

    // Parse keywords from keyword part
    if let Some(kw_text) = keyword_part {
        for part in split_keyword_list(kw_text.trim().trim_end_matches('.')) {
            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::AddKeyword { keyword: kw });
            }
        }
    }

    modifications
}

fn parse_conditional_static(text: &str) -> Option<StaticDefinition> {
    let conditional = text.strip_prefix("As long as ")?;
    let (condition_text, remainder) = conditional.split_once(", ")?;

    let condition =
        parse_static_condition(condition_text).unwrap_or(StaticCondition::Unrecognized {
            text: condition_text.to_string(),
        });

    let mut def = parse_static_line(remainder.trim())?;
    if def.condition.is_some() {
        return None;
    }
    def.condition = Some(condition);
    def.description = Some(text.to_string());
    Some(def)
}

/// Parse a condition clause (the text between "As long as" and the comma).
///
/// Returns a typed `StaticCondition` for known patterns, or `None` if the
/// condition text is not recognized. Callers may fall back to `Unrecognized`.
///
/// Supported patterns:
/// - "you have at least N life more than your starting life total" → LifeMoreThanStartingBy
/// - "your devotion to [colors] is less than N" → DevotionGE (with inverted threshold)
/// - "it's your turn" → DuringYourTurn
/// - "you control a/an [type]" → IsPresent with filter
fn parse_static_condition(text: &str) -> Option<StaticCondition> {
    let lower = text.to_lowercase();

    // "you have at least N life more than your starting life total"
    if let Some(amount_text) = lower
        .strip_prefix("you have at least ")
        .and_then(|s| s.strip_suffix(" life more than your starting life total"))
    {
        let (amount, rest) = parse_number(amount_text)?;
        if rest.trim().is_empty() {
            return Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeAboveStarting,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed {
                    value: amount as i32,
                },
            });
        }
    }

    // "it's your turn"
    if lower == "it's your turn" {
        return Some(StaticCondition::DuringYourTurn);
    }

    // "your devotion to [color(s)] is less than N" (Theros gods)
    // Note: "less than N" is stored as DevotionGE with the same threshold —
    // the *effect* typically removes creature type, so the condition being false
    // (devotion >= N) means the removal doesn't apply and the god IS a creature.
    if let Some(condition) = parse_devotion_condition(&lower) {
        return Some(condition);
    }

    // "you control a/an [type]" → IsPresent
    if let Some(condition) = parse_control_presence_condition(&lower) {
        return Some(condition);
    }

    // "the number of [quantity] is [comparator] [quantity]"
    if let Some(condition) = parse_quantity_comparison(&lower) {
        return Some(condition);
    }

    // "the chosen color is [color]"
    if let Some(color_name) = lower.strip_prefix("the chosen color is ") {
        use crate::types::mana::ManaColor;
        let color = match color_name.trim().trim_end_matches('.') {
            "white" => Some(ManaColor::White),
            "blue" => Some(ManaColor::Blue),
            "black" => Some(ManaColor::Black),
            "red" => Some(ManaColor::Red),
            "green" => Some(ManaColor::Green),
            _ => None,
        };
        if let Some(c) = color {
            return Some(StaticCondition::ChosenColorIs { color: c });
        }
    }

    None
}

/// Parse "your devotion to [color(s)] is less than N" or "is N or greater".
fn parse_devotion_condition(lower: &str) -> Option<StaticCondition> {
    let rest = lower.strip_prefix("your devotion to ")?;

    // Split at " is " to get colors and comparison
    let (color_text, comparison) = rest.split_once(" is ")?;

    // Parse colors: "white", "blue and red", "white and black"
    let colors = parse_color_list(color_text)?;

    // Parse comparison: "less than N" or "N or greater"
    let threshold = if let Some(n_text) = comparison.strip_prefix("less than ") {
        parse_number(n_text.trim())?.0
    } else if let Some(n_rest) = comparison.strip_suffix(" or greater") {
        parse_number(n_rest.trim())?.0
    } else {
        return None;
    };

    Some(StaticCondition::DevotionGE { colors, threshold })
}

/// Parse "you control a/an [type/subtype]" into IsPresent.
fn parse_control_presence_condition(lower: &str) -> Option<StaticCondition> {
    let rest = lower
        .strip_prefix("you control a ")
        .or_else(|| lower.strip_prefix("you control an "))?;

    // Try to parse the rest as a type phrase
    let filter = parse_presence_filter(rest)?;

    Some(StaticCondition::IsPresent {
        filter: Some(TargetFilter::Typed(filter.controller(ControllerRef::You))),
    })
}

/// Parse a simple type/subtype/color description into a TypedFilter.
fn parse_presence_filter(text: &str) -> Option<TypedFilter> {
    use crate::types::ability::TypeFilter;

    let trimmed = text.trim().trim_end_matches('.');

    // "[color] or [color] permanent" — color-based presence check
    if let Some(perm_prefix) = trimmed.strip_suffix(" permanent") {
        let colors: Vec<&str> = perm_prefix.split(" or ").collect();
        if colors.len() >= 2 {
            // Multiple color options — we'd need an Or filter; for now handle as simple card match
            return Some(TypedFilter::card());
        }
    }

    // "creature with power N or greater/less/more"
    // Reuses parse_number so both digits and words ("four") are handled.
    if let Some(rest) = trimmed.strip_prefix("creature with power ") {
        let (n, remainder) = parse_number(rest)?;
        let prop = match remainder.trim() {
            "or greater" | "or more" => FilterProp::PowerGE { value: n as i32 },
            "or less" => FilterProp::PowerLE { value: n as i32 },
            _ => return None,
        };
        return Some(TypedFilter::creature().properties(vec![prop]));
    }

    // Simple core types
    let type_filter = match trimmed {
        "artifact" => Some(TypeFilter::Artifact),
        "creature" => Some(TypeFilter::Creature),
        "enchantment" => Some(TypeFilter::Enchantment),
        "land" => Some(TypeFilter::Land),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        _ => None,
    };

    if let Some(tf) = type_filter {
        return Some(TypedFilter::new(tf));
    }

    // Subtype-based: "you control a Demon", "you control an Elf"
    if !trimmed.is_empty() && trimmed.chars().next().unwrap().is_uppercase() {
        return Some(TypedFilter::creature().subtype(trimmed.to_string()));
    }

    None
}

/// Parse a color list like "white", "blue and red", "white, blue, and black".
fn parse_color_list(text: &str) -> Option<Vec<crate::types::mana::ManaColor>> {
    use crate::types::mana::ManaColor;

    let color_from_name = |s: &str| -> Option<ManaColor> {
        match s.trim() {
            "white" => Some(ManaColor::White),
            "blue" => Some(ManaColor::Blue),
            "black" => Some(ManaColor::Black),
            "red" => Some(ManaColor::Red),
            "green" => Some(ManaColor::Green),
            _ => None,
        }
    };

    // Try single color first
    if let Some(c) = color_from_name(text) {
        return Some(vec![c]);
    }

    // "X and Y"
    if let Some((a, b)) = text.split_once(" and ") {
        let mut colors = Vec::new();
        // Handle "X, Y, and Z" — a would be "X, Y" and b would be "Z"
        for part in a.split(", ") {
            colors.push(color_from_name(part)?);
        }
        colors.push(color_from_name(b)?);
        return Some(colors);
    }

    None
}

/// Parse "the number of [quantity] is [comparator] [quantity]" into a QuantityComparison.
fn parse_quantity_comparison(lower: &str) -> Option<StaticCondition> {
    let rest = lower.strip_prefix("the number of ")?;
    let (lhs_text, comparison) = rest.split_once(" is ")?;
    let lhs = parse_quantity_ref(lhs_text)?;
    let (comparator, rhs_text) = parse_comparator_prefix(comparison)?;
    let rhs = parse_quantity_ref(rhs_text.trim())?;
    Some(StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref { qty: lhs },
        comparator,
        rhs: QuantityExpr::Ref { qty: rhs },
    })
}

/// Map a quantity phrase to a dynamic QuantityRef.
pub(super) fn parse_quantity_ref(text: &str) -> Option<QuantityRef> {
    let trimmed = text.trim().trim_end_matches('.');
    match trimmed {
        "cards in your hand" => Some(QuantityRef::HandSize),
        "your life total" => Some(QuantityRef::LifeTotal),
        "cards in your graveyard" => Some(QuantityRef::GraveyardSize),
        // CR 208.3: Self-referential P/T lookups.
        "~'s power" | "its power" | "this creature's power" => Some(QuantityRef::SelfPower),
        "~'s toughness" | "its toughness" | "this creature's toughness" => {
            Some(QuantityRef::SelfToughness)
        }
        _ => {
            // "[counter type] counters on ~" / "[counter type] counters on it"
            if let Some(rest) = trimmed
                .strip_suffix(" counters on ~")
                .or_else(|| trimmed.strip_suffix(" counters on it"))
            {
                let counter_type = rest
                    .strip_prefix("the number of ")
                    .unwrap_or(rest)
                    .trim()
                    .replace('+', "plus")
                    .replace('-', "minus");
                if !counter_type.is_empty() {
                    return Some(QuantityRef::CountersOnSelf { counter_type });
                }
            }

            // "the greatest power among {type phrase}" → Aggregate { Max, Power, filter }
            if let Some(rest) = trimmed.strip_prefix("the greatest power among ") {
                let (filter, _) = parse_type_phrase(rest);
                if !matches!(filter, TargetFilter::Any) {
                    return Some(QuantityRef::Aggregate {
                        function: AggregateFunction::Max,
                        property: ObjectProperty::Power,
                        filter,
                    });
                }
            }
            // "the greatest toughness among {type phrase}"
            if let Some(rest) = trimmed.strip_prefix("the greatest toughness among ") {
                let (filter, _) = parse_type_phrase(rest);
                if !matches!(filter, TargetFilter::Any) {
                    return Some(QuantityRef::Aggregate {
                        function: AggregateFunction::Max,
                        property: ObjectProperty::Toughness,
                        filter,
                    });
                }
            }
            // "the greatest mana value among {type phrase}"
            if let Some(rest) = trimmed.strip_prefix("the greatest mana value among ") {
                let (filter, _) = parse_type_phrase(rest);
                if !matches!(filter, TargetFilter::Any) {
                    return Some(QuantityRef::Aggregate {
                        function: AggregateFunction::Max,
                        property: ObjectProperty::ManaValue,
                        filter,
                    });
                }
            }
            // "the total power of {type phrase}"
            if let Some(rest) = trimmed.strip_prefix("the total power of ") {
                let (filter, _) = parse_type_phrase(rest);
                if !matches!(filter, TargetFilter::Any) {
                    return Some(QuantityRef::Aggregate {
                        function: AggregateFunction::Sum,
                        property: ObjectProperty::Power,
                        filter,
                    });
                }
            }

            // "the number of {type} you control" → ObjectCount { filter }
            // "the number of opponents you have" → PlayerCount { Opponent }
            if let Some(rest) = trimmed.strip_prefix("the number of ") {
                if rest == "opponents you have" || rest == "opponent you have" {
                    return Some(QuantityRef::PlayerCount {
                        filter: PlayerFilter::Opponent,
                    });
                }
                let (filter, _) = parse_type_phrase(rest);
                if !matches!(filter, TargetFilter::Any) {
                    return Some(QuantityRef::ObjectCount { filter });
                }
            }
            // "your devotion to {color}" / "your devotion to {color} and {color}"
            if let Some(rest) = trimmed.strip_prefix("your devotion to ") {
                let colors = parse_devotion_colors(rest);
                if !colors.is_empty() {
                    return Some(QuantityRef::Devotion { colors });
                }
            }
            None
        }
    }
}

/// Parse color names from a devotion phrase like "black", "black and red".
fn parse_devotion_colors(text: &str) -> Vec<ManaColor> {
    text.split(" and ")
        .filter_map(|word| {
            let capitalized = capitalize_first(word.trim());
            ManaColor::from_str(&capitalized).ok()
        })
        .collect()
}

/// Capitalize the first letter of a word (for ManaColor::from_str).
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Strip a comparator prefix from a comparison clause, returning (Comparator, remainder).
fn parse_comparator_prefix(text: &str) -> Option<(Comparator, &str)> {
    // Longer prefixes must be tried before shorter ones to avoid partial matches.
    if let Some(rest) = text.strip_prefix("greater than or equal to ") {
        return Some((Comparator::GE, rest));
    }
    if let Some(rest) = text.strip_prefix("less than or equal to ") {
        return Some((Comparator::LE, rest));
    }
    if let Some(rest) = text.strip_prefix("greater than ") {
        return Some((Comparator::GT, rest));
    }
    if let Some(rest) = text.strip_prefix("less than ") {
        return Some((Comparator::LT, rest));
    }
    if let Some(rest) = text.strip_prefix("equal to ") {
        return Some((Comparator::EQ, rest));
    }
    None
}

fn find_continuous_predicate_start(lower: &str) -> Option<usize> {
    [
        " gets ", " get ", " gains ", " gain ", " has ", " have ", " loses ", " lose ",
    ]
    .into_iter()
    .filter_map(|marker| lower.find(marker))
    .min()
}

fn parse_continuous_subject_filter(subject: &str) -> Option<TargetFilter> {
    let trimmed = subject.trim();
    let lower = trimmed.to_lowercase();

    // Strip "Each " prefix — "Each creature you control" is semantically identical to
    // "Creatures you control" for filter purposes.
    if lower.starts_with("each ") {
        return parse_continuous_subject_filter(trimmed[5..].trim());
    }

    if lower.starts_with("other ") {
        let original_rest = trimmed[6..].trim();
        return parse_continuous_subject_filter(original_rest).map(add_another_filter);
    }

    if let Some(filter) = parse_modified_creature_subject_filter(trimmed) {
        return Some(filter);
    }

    if let Some(filter) = parse_creature_subject_filter(trimmed) {
        return Some(filter);
    }

    parse_rule_static_subject_filter(trimmed)
}

/// Try to strip a leading "with [counter] counter(s) on it/them" clause from `text`,
/// returning the `FilterProp` and the remaining text after the clause.
/// CR 613.1 + CR 613.7: Used to parse conditional static keyword grants in layer 6.
fn strip_counter_condition_prefix(text: &str) -> Option<(FilterProp, &str)> {
    let lower = text.to_lowercase();
    if !lower.starts_with("with ") {
        return None;
    }
    // parse_counter_suffix expects optional leading whitespace before "with"
    let (prop, consumed) = parse_counter_suffix(&lower)?;
    Some((prop, text[consumed..].trim_start()))
}

fn parse_modified_creature_subject_filter(subject: &str) -> Option<TargetFilter> {
    let lower = subject.to_lowercase();
    if lower == "equipped creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
        ));
    }

    let controlled_patterns = [
        ("tapped creatures you control", FilterProp::Tapped),
        ("attacking creatures you control", FilterProp::Attacking),
        ("equipped creatures you control", FilterProp::EquippedBy),
    ];

    for (pattern, property) in controlled_patterns {
        if lower == pattern {
            return Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![property]),
            ));
        }
    }

    if lower == "attacking creatures" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::Attacking]),
        ));
    }

    None
}

fn parse_creature_subject_filter(subject: &str) -> Option<TargetFilter> {
    let trimmed = subject.trim();
    let lower = trimmed.to_lowercase();

    let descriptor = if let Some(prefix) = trimmed.strip_suffix(" creatures") {
        prefix.trim()
    } else if !trimmed.contains(' ') && lower.ends_with('s') {
        trimmed.trim_end_matches('s').trim()
    } else {
        return None;
    };

    if descriptor.is_empty() {
        return None;
    }

    if let Some(color) = parse_named_color(descriptor) {
        return Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![FilterProp::HasColor {
                color: color.to_string(),
            }],
        )));
    }

    if is_capitalized_words(descriptor) {
        let subtype = descriptor.to_string();
        return Some(TargetFilter::Typed(
            TypedFilter::creature().subtype(subtype),
        ));
    }

    None
}

fn add_another_filter(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed.properties.push(FilterProp::Another);
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(add_another_filter).collect(),
        },
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Another])),
            ],
        },
    }
}

/// Add a single `FilterProp` to an existing `TargetFilter`.
fn add_property(filter: TargetFilter, prop: FilterProp) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed.properties.push(prop);
            TargetFilter::Typed(typed)
        }
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::default().properties(vec![prop])),
            ],
        },
    }
}

fn strip_rule_static_subject<'a>(text: &'a str, lower: &str) -> Option<(TargetFilter, &'a str)> {
    for marker in [
        " doesn't untap during ",
        " doesn’t untap during ",
        " don't untap during ",
        " don’t untap during ",
        " attacks each combat if able",
        " can block only creatures with flying",
        " has shroud",
        " have shroud",
        " has no maximum hand size",
        " have no maximum hand size",
        " may play an additional land",
        " may play up to ",
        " may look at the top card of your library",
        " loses all abilities",
        " lose all abilities",
    ] {
        let Some(subject_end) = lower.find(marker) else {
            continue;
        };
        let subject = text[..subject_end].trim();
        let predicate = text[subject_end + 1..].trim();
        let affected = parse_rule_static_subject_filter(subject)?;
        return Some((affected, predicate));
    }

    None
}

fn parse_rule_static_subject_filter(subject: &str) -> Option<TargetFilter> {
    let lower = subject.to_lowercase();

    if matches!(
        lower.as_str(),
        "~" | "this"
            | "it"
            | "this card"
            | "this creature"
            | "this permanent"
            | "this artifact"
            | "this land"
    ) {
        return Some(TargetFilter::SelfRef);
    }

    if lower == "you" {
        return Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ));
    }

    if matches!(lower.as_str(), "players" | "each player") {
        return Some(TargetFilter::Player);
    }

    if lower == "enchanted creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ));
    }

    if lower == "enchanted permanent" {
        return Some(TargetFilter::Typed(
            TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy]),
        ));
    }

    if lower == "equipped creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
        ));
    }

    let (filter, rest) = parse_type_phrase(subject);
    if rest.trim().is_empty() {
        return Some(filter);
    }

    None
}

fn parse_rule_static_predicate(text: &str) -> Option<RuleStaticPredicate> {
    let lower = text.to_lowercase();

    if lower.starts_with("doesn't untap during")
        || lower.starts_with("doesn\u{2019}t untap during")
        || lower.starts_with("don't untap during")
        || lower.starts_with("don\u{2019}t untap during")
    {
        return Some(RuleStaticPredicate::CantUntap);
    }

    if matches!(
        lower.as_str(),
        "attacks each combat if able" | "attacks each combat if able."
    ) {
        return Some(RuleStaticPredicate::MustAttack);
    }

    if matches!(
        lower.as_str(),
        "can block only creatures with flying" | "can block only creatures with flying."
    ) {
        return Some(RuleStaticPredicate::BlockOnlyCreaturesWithFlying);
    }

    if matches!(
        lower.as_str(),
        "has shroud" | "has shroud." | "have shroud" | "have shroud."
    ) {
        return Some(RuleStaticPredicate::Shroud);
    }

    if lower.starts_with("may look at the top card of your library") {
        return Some(RuleStaticPredicate::MayLookAtTopOfLibrary);
    }

    if matches!(
        lower.as_str(),
        "lose all abilities"
            | "lose all abilities."
            | "loses all abilities"
            | "loses all abilities."
    ) {
        return Some(RuleStaticPredicate::LoseAllAbilities);
    }

    if matches!(
        lower.as_str(),
        "has no maximum hand size"
            | "has no maximum hand size."
            | "have no maximum hand size"
            | "have no maximum hand size."
    ) {
        return Some(RuleStaticPredicate::NoMaximumHandSize);
    }

    if lower.starts_with("may play an additional land")
        || (lower.starts_with("may play up to ") && lower.contains("additional land"))
    {
        return Some(RuleStaticPredicate::MayPlayAdditionalLand);
    }

    None
}

fn lower_rule_static(
    predicate: RuleStaticPredicate,
    affected: TargetFilter,
    description: &str,
) -> StaticDefinition {
    match predicate {
        RuleStaticPredicate::CantUntap => {
            StaticDefinition::new(StaticMode::Other("CantUntap".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::MustAttack => StaticDefinition::new(StaticMode::MustAttack)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::BlockOnlyCreaturesWithFlying => {
            StaticDefinition::new(StaticMode::Other("BlockRestriction".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::Shroud => {
            StaticDefinition::new(StaticMode::Other("Shroud".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::MayLookAtTopOfLibrary => {
            StaticDefinition::new(StaticMode::Other("MayLookAtTopOfLibrary".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::LoseAllAbilities => StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::RemoveAllAbilities])
            .description(description.to_string()),
        RuleStaticPredicate::NoMaximumHandSize => {
            StaticDefinition::new(StaticMode::Other("NoMaximumHandSize".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::MayPlayAdditionalLand => {
            StaticDefinition::new(StaticMode::Other("MayPlayAdditionalLand".to_string()))
                .affected(affected)
                .description(description.to_string())
        }
    }
}

fn parse_named_color(text: &str) -> Option<&'static str> {
    match text.trim().to_ascii_lowercase().as_str() {
        "white" => Some("White"),
        "blue" => Some("Blue"),
        "black" => Some("Black"),
        "red" => Some("Red"),
        "green" => Some("Green"),
        _ => None,
    }
}

/// Check that a string is one or more capitalized words.
fn is_capitalized_words(s: &str) -> bool {
    let trimmed = s.trim();
    !trimmed.is_empty()
        && trimmed
            .split_whitespace()
            .all(|w| w.chars().next().is_some_and(|c| c.is_uppercase()))
}

/// Parse "gets +N/+M [and has {keyword}]" after the subject.
fn parse_continuous_gets_has(
    text: &str,
    affected: TargetFilter,
    description: &str,
) -> Option<StaticDefinition> {
    let modifications = parse_continuous_modifications(text);

    if modifications.is_empty() {
        return None;
    }

    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications)
            .description(description.to_string()),
    )
}

pub(crate) fn parse_continuous_modifications(text: &str) -> Vec<ContinuousModification> {
    let lower = text.to_lowercase();
    let mut modifications = Vec::new();

    if lower.contains("lose all abilities") {
        modifications.push(ContinuousModification::RemoveAllAbilities);
    }

    if lower.starts_with("gets ") || lower.starts_with("get ") {
        let offset = if lower.starts_with("gets ") { 5 } else { 4 };
        let after = &text[offset..].trim();
        if let Some((p, t)) = parse_pt_mod(after) {
            modifications.push(ContinuousModification::AddPower { value: p });
            modifications.push(ContinuousModification::AddToughness { value: t });
        }
    }

    if let Some((power, toughness)) = parse_base_pt_mod(text) {
        modifications.push(ContinuousModification::SetPower { value: power });
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }
    if let Some(power) = parse_base_power_mod(text) {
        modifications.push(ContinuousModification::SetPower { value: power });
    }
    if let Some(toughness) = parse_base_toughness_mod(text) {
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }

    for definition in parse_quoted_abilities(text) {
        modifications.push(ContinuousModification::GrantAbility {
            definition: Box::new(definition),
        });
    }

    if let Some(keyword_text) = extract_keyword_clause(text) {
        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::AddKeyword { keyword: kw });
            }
        }
    }

    // CR 702: "lose [keyword]" / "loses [keyword]" — keyword removal.
    if let Some(keyword_text) = extract_lose_keyword_clause(text) {
        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::RemoveKeyword { keyword: kw });
            }
        }
    }

    modifications
}

fn parse_base_pt_mod(text: &str) -> Option<(i32, i32)> {
    let lower = text.to_lowercase();
    let pos = lower.find("base power and toughness ")?;
    let pt_text = text[pos + "base power and toughness ".len()..].trim();
    parse_pt_mod(pt_text)
}

fn parse_base_power_mod(text: &str) -> Option<i32> {
    let lower = text.to_lowercase();
    if lower.contains("base power and toughness ") {
        return None;
    }
    let pos = lower.find("base power ")?;
    let power_text = text[pos + "base power ".len()..].trim();
    parse_single_pt_value(power_text)
}

fn parse_base_toughness_mod(text: &str) -> Option<i32> {
    let lower = text.to_lowercase();
    if lower.contains("base power and toughness ") {
        return None;
    }
    let pos = lower.find("base toughness ")?;
    let toughness_text = text[pos + "base toughness ".len()..].trim();
    parse_single_pt_value(toughness_text)
}

fn parse_single_pt_value(text: &str) -> Option<i32> {
    let value = text
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | ','))
        .next()?;
    value.replace('+', "").parse::<i32>().ok()
}

/// Extract quoted ability text from Oracle text and parse each into a typed AbilityDefinition.
///
/// Quoted abilities like `"{T}: Add two mana of any one color."` are parsed by splitting
/// at the cost separator (`:` after mana/tap symbols) and reusing `parse_oracle_cost` +
/// `parse_effect_chain`. Non-activated quoted text is parsed as a spell-like effect chain.
fn parse_quoted_abilities(text: &str) -> Vec<AbilityDefinition> {
    let mut definitions = Vec::new();
    let mut start = None;

    for (idx, ch) in text.char_indices() {
        if ch == '"' {
            if let Some(open) = start.take() {
                let ability_text = text[open + 1..idx].trim();
                if !ability_text.is_empty() {
                    definitions.push(parse_quoted_ability(ability_text));
                }
            } else {
                start = Some(idx);
            }
        }
    }

    definitions
}

/// Parse a single quoted ability string into a typed AbilityDefinition.
///
/// If the text contains a cost separator (e.g., `{T}: ...`), it's treated as an
/// activated ability with the cost parsed separately. Otherwise it's treated as
/// a spell-like effect.
fn parse_quoted_ability(text: &str) -> AbilityDefinition {
    // Find the cost/effect separator — look for ": " after a cost-like prefix
    // (mana symbols, {T}, loyalty, etc.)
    if let Some(colon_pos) = find_cost_separator(text) {
        let cost_text = text[..colon_pos].trim();
        let effect_text = text[colon_pos + 1..].trim();
        let cost = parse_oracle_cost(cost_text);
        let mut def = parse_effect_chain(effect_text, AbilityKind::Activated);
        def.cost = Some(cost);
        def.description = Some(text.to_string());
        def
    } else {
        // No cost separator — treat as spell-like ability text
        let mut def = parse_effect_chain(text, AbilityKind::Spell);
        def.description = Some(text.to_string());
        def
    }
}

/// Find the position of the cost/effect separator colon in ability text.
///
/// Looks for `: ` or `:\n` that appears after cost-like content (mana symbols,
/// {T}, numeric loyalty). Returns the byte offset of the colon, or None.
fn find_cost_separator(text: &str) -> Option<usize> {
    // Walk through looking for ':' that follows a closing brace or known cost prefix
    for (idx, ch) in text.char_indices() {
        if ch == ':' && idx > 0 {
            let prefix = &text[..idx];
            // Must have cost-like content before the colon
            let has_cost = prefix.contains('{')
                || prefix.trim().parse::<i32>().is_ok()
                || prefix.trim().starts_with('+')
                || prefix.trim().starts_with('\u{2212}'); // minus sign for loyalty
            if has_cost {
                return Some(idx);
            }
        }
    }
    None
}

/// CR 702: Split a keyword list like "flying and first strike" into individual keywords.
fn split_keyword_list(text: &str) -> Vec<Cow<'_, str>> {
    let text = text.trim().trim_end_matches('.');
    // Split on ", and ", " and ", or ", "
    let mut parts: Vec<&str> = Vec::new();
    for chunk in text.split(", and ") {
        for sub in chunk.split(" and ") {
            for item in sub.split(", ") {
                let trimmed = item.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed);
                }
            }
        }
    }
    // CR 702.16: Expand "protection from X and from Y" into separate entries.
    // Reuses the building block from oracle_keyword.rs which handles inline,
    // comma-continuation, and Oxford comma protection patterns.
    super::oracle_keyword::expand_protection_parts(&parts)
}

fn extract_keyword_clause(text: &str) -> Option<&str> {
    let lower = text.to_lowercase();

    for needle in [
        " and gains ",
        " and gain ",
        " and has ",
        " and have ",
        " gains ",
        " gain ",
        " has ",
        " have ",
    ] {
        if let Some(pos) = lower.find(needle) {
            return Some(&text[pos + needle.len()..]);
        }
    }

    for prefix in ["gains ", "gain ", "has ", "have "] {
        if lower.starts_with(prefix) {
            return Some(&text[prefix.len()..]);
        }
    }

    None
}

/// Extract the keyword text from "lose [keyword]" / "loses [keyword]" clauses.
/// Mirrors `extract_keyword_clause` but for keyword removal.
fn extract_lose_keyword_clause(text: &str) -> Option<&str> {
    let lower = text.to_lowercase();

    for needle in [" and loses ", " and lose "] {
        if let Some(pos) = lower.find(needle) {
            let after = &text[pos + needle.len()..];
            // Stop before "and gains" to avoid consuming the gain clause
            let end = lower[pos + needle.len()..]
                .find(" and gain")
                .unwrap_or(after.len());
            return Some(&after[..end]);
        }
    }

    for prefix in ["loses ", "lose "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let after = &text[prefix.len()..];
            // Stop before "and gains"/"and gain" to avoid consuming the gain clause
            let end = rest.find(" and gain").unwrap_or(after.len());
            return Some(&after[..end]);
        }
    }

    None
}

fn parse_pt_mod(text: &str) -> Option<(i32, i32)> {
    let text = text.trim();
    let slash = text.find('/')?;
    let p_str = &text[..slash];
    let rest = &text[slash + 1..];
    let t_end = rest
        .find(|c: char| c.is_whitespace() || c == '.' || c == ',')
        .unwrap_or(rest.len());
    let t_str = &rest[..t_end];
    let p = p_str.replace('+', "").parse::<i32>().ok()?;
    let t = t_str.replace('+', "").parse::<i32>().ok()?;
    Some((p, t))
}

/// Map a keyword text to a Keyword enum variant using the FromStr impl.
/// Returns None only for `Keyword::Unknown`.
fn map_keyword(text: &str) -> Option<Keyword> {
    let word = text.trim().trim_end_matches('.').trim();
    if word.is_empty() {
        return None;
    }
    if let Some(keyword) = parse_landwalk_keyword(word) {
        return Some(keyword);
    }
    match Keyword::from_str(word) {
        Ok(Keyword::Unknown(_)) => {
            // Fall through to Oracle-format parser for parameterized keywords
            // like "protection from red" that use spaces instead of colons.
            super::oracle_keyword::parse_keyword_from_oracle(word)
        }
        Ok(kw) => Some(kw),
        Err(_) => None, // Infallible, but satisfy the compiler
    }
}

fn parse_landwalk_keyword(text: &str) -> Option<Keyword> {
    match text.trim().to_ascii_lowercase().as_str() {
        "plainswalk" => Some(Keyword::Landwalk("Plains".to_string())),
        "islandwalk" => Some(Keyword::Landwalk("Island".to_string())),
        "swampwalk" => Some(Keyword::Landwalk("Swamp".to_string())),
        "mountainwalk" => Some(Keyword::Landwalk("Mountain".to_string())),
        "forestwalk" => Some(Keyword::Landwalk("Forest".to_string())),
        _ => None,
    }
}

/// Parse CDA power/toughness equality patterns like:
/// - "~'s power and toughness are each equal to the number of creatures you control."
/// - "~'s power is equal to the number of card types among cards in all graveyards
///   and its toughness is equal to that number plus 1."
/// - "~'s toughness is equal to the number of cards in your hand."
fn parse_cda_pt_equality(lower: &str, text: &str) -> Option<StaticDefinition> {
    // Detect framing
    let both = lower.contains("power and toughness are each equal to");
    let power_only = !both && lower.contains("power is equal to");
    let toughness_only = !both && !power_only && lower.contains("toughness is equal to");

    if !both && !power_only && !toughness_only {
        return None;
    }

    // Extract the quantity text after "equal to "
    let quantity_start = if both {
        lower
            .find("are each equal to ")
            .map(|p| p + "are each equal to ".len())
    } else if power_only {
        lower
            .find("power is equal to ")
            .map(|p| p + "power is equal to ".len())
    } else {
        lower
            .find("toughness is equal to ")
            .map(|p| p + "toughness is equal to ".len())
    };
    let quantity_text = &lower[quantity_start?..];

    // Strip trailing clause for split P/T ("and its toughness is equal to...")
    let quantity_text = quantity_text
        .split(" and its toughness")
        .next()
        .unwrap_or(quantity_text)
        .trim_end_matches('.');

    let qty = parse_cda_quantity(quantity_text)?;

    let mut modifications = Vec::new();

    if both {
        modifications.push(ContinuousModification::SetDynamicPower { value: qty.clone() });
        modifications.push(ContinuousModification::SetDynamicToughness { value: qty });
    } else if power_only {
        modifications.push(ContinuousModification::SetDynamicPower { value: qty.clone() });
        // Check for split P/T: "and its toughness is equal to that number plus N"
        if let Some(plus_pos) = lower.find("that number plus ") {
            let after_plus = &lower[plus_pos + "that number plus ".len()..];
            let n_str = after_plus
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap_or("0");
            let offset = n_str.parse::<i32>().unwrap_or(0);
            modifications.push(ContinuousModification::SetDynamicToughness {
                value: QuantityExpr::Offset {
                    inner: Box::new(qty),
                    offset,
                },
            });
        }
    } else {
        // toughness_only
        modifications.push(ContinuousModification::SetDynamicToughness { value: qty });
    }

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(modifications)
            .cda()
            .description(text.to_string()),
    )
}

/// Parse a CDA quantity phrase into a `QuantityExpr`.
/// Handles patterns like:
/// - "the number of creatures you control"
/// - "the number of cards in your hand"
/// - "your life total"
/// - "the number of creature cards in your graveyard"
/// - "the number of card types among cards in all graveyards"
/// - "the number of basic land types among lands you control"
/// - "N plus the number of X"
pub(crate) fn parse_cda_quantity(text: &str) -> Option<QuantityExpr> {
    let text = text.trim().trim_end_matches('.');

    // "twice [inner]" → Multiply { factor: 2, inner }
    if let Some(rest) = text.strip_prefix("twice ") {
        if let Some(inner) = parse_cda_quantity(rest) {
            return Some(QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(inner),
            });
        }
    }

    // "three times [inner]" → Multiply { factor: 3, inner }
    if let Some(rest) = text.strip_prefix("three times ") {
        if let Some(inner) = parse_cda_quantity(rest) {
            return Some(QuantityExpr::Multiply {
                factor: 3,
                inner: Box::new(inner),
            });
        }
    }

    // "N plus [inner]" generalized offset pattern
    if let Some((prefix, rest)) = text.split_once(" plus ") {
        if let Some((n, _)) = parse_number(prefix) {
            if let Some(inner) = parse_cda_quantity(rest) {
                return Some(QuantityExpr::Offset {
                    inner: Box::new(inner),
                    offset: n as i32,
                });
            }
        }
    }

    // "N plus the number of X" offset pattern (legacy, specific)
    if let Some(rest) = text.strip_suffix(" plus the number of cards in your hand") {
        if let Some((n, _)) = parse_number(rest) {
            return Some(QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::HandSize,
                }),
                offset: n as i32,
            });
        }
    }

    // "the number of card types among cards in all graveyards"
    if text.contains("card types among cards in all graveyards") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::CardTypesInGraveyards {
                scope: CountScope::All,
            },
        });
    }
    if text.contains("card types among cards in your graveyard") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::CardTypesInGraveyards {
                scope: CountScope::Controller,
            },
        });
    }

    // "the number of basic land types among lands you control" (Domain)
    if text.contains("basic land types among lands you control") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::BasicLandTypeCount,
        });
    }

    // "the number of cards in your hand"
    if text.contains("cards in your hand") || text == "the number of cards in your hand" {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::HandSize,
        });
    }

    // "your life total"
    if text.contains("your life total") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::LifeTotal,
        });
    }

    // "the number of cards in your graveyard"
    if text == "the number of cards in your graveyard" || text.contains("cards in your graveyard") {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::ZoneCardCount {
                zone: ZoneRef::Graveyard,
                card_types: vec![],
                scope: CountScope::Controller,
            },
        });
    }

    // "the number of {type} cards in your graveyard"
    if let Some(rest) = text.strip_prefix("the number of ") {
        if let Some(type_text) = rest.strip_suffix(" cards in your graveyard") {
            if let Some(tf) = parse_cda_type_filter(type_text) {
                return Some(QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Graveyard,
                        card_types: vec![tf],
                        scope: CountScope::Controller,
                    },
                });
            }
        }
        if let Some(type_text) = rest.strip_suffix(" cards in all graveyards") {
            if let Some(tf) = parse_cda_type_filter(type_text) {
                return Some(QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Graveyard,
                        card_types: vec![tf],
                        scope: CountScope::All,
                    },
                });
            }
        }
    }

    // Delegate to existing parse_quantity_ref for patterns like
    // "the number of {type} you control", "your devotion to X"
    if let Some(qty) = parse_quantity_ref(text) {
        return Some(QuantityExpr::Ref { qty });
    }

    None
}

/// Map a type word to a `TypeFilter` for CDA zone card counting.
fn parse_cda_type_filter(text: &str) -> Option<TypeFilter> {
    match text.trim() {
        "creature" => Some(TypeFilter::Creature),
        "instant" => Some(TypeFilter::Instant),
        "sorcery" => Some(TypeFilter::Sorcery),
        "land" => Some(TypeFilter::Land),
        "artifact" => Some(TypeFilter::Artifact),
        "enchantment" => Some(TypeFilter::Enchantment),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        "instant and sorcery" | "instant or sorcery" => None, // Needs Vec, handled separately
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::TypeFilter;

    #[test]
    fn static_bonesplitter() {
        let def = parse_static_line("Equipped creature gets +2/+0.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 0 }));
    }

    #[test]
    fn static_rancor() {
        let def = parse_static_line("Enchanted creature gets +2/+0 and has trample.").unwrap();
        assert!(def.modifications.len() >= 3); // +2, +0, trample
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample
            }));
    }

    #[test]
    fn static_cant_be_blocked() {
        let def =
            parse_static_line("Questing Beast can't be blocked by creatures with power 2 or less.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantBeBlocked".to_string()));
    }

    #[test]
    fn static_creatures_you_control() {
        let def = parse_static_line("Creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                ..
            }))
        ));
    }

    // --- New pattern tests ---

    #[test]
    fn static_self_referential_has_keyword() {
        let def = parse_static_line("Phage the Untouchable has deathtouch.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Deathtouch,
            }));
    }

    #[test]
    fn static_enchanted_permanent() {
        let def = parse_static_line("Enchanted permanent has hexproof.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Permanent),
                ..
            }))
        ));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }));
    }

    #[test]
    fn static_all_creatures() {
        let def = parse_static_line("All creatures get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Creature),
                controller: None,
                ..
            }))
        ));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
    }

    #[test]
    fn static_subtype_creatures_you_control() {
        let def = parse_static_line("Elf creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Creature),
                subtype: Some(ref s),
                controller: Some(ControllerRef::You),
                ..
            })) if s == "Elf"
        ));
    }

    #[test]
    fn static_color_creatures_you_control() {
        let def = parse_static_line("White creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties,
            })) if properties == vec![FilterProp::HasColor {
                color: "White".to_string(),
            }]
        ));
    }

    #[test]
    fn static_other_subtype_you_control() {
        let def = parse_static_line("Other Zombies you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
    }

    #[test]
    fn static_cant_block() {
        let def = parse_static_line("Ragavan can't block.").unwrap();
        assert_eq!(def.mode, StaticMode::CantBlock);
        assert!(def.modifications.is_empty());
        assert!(def.description.is_some());
    }

    #[test]
    fn static_doesnt_untap() {
        let def =
            parse_static_line("Darksteel Sentinel doesn't untap during your untap step.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantUntap".to_string()));
        assert!(def.description.is_some());
    }

    #[test]
    fn static_cant_be_countered() {
        let def = parse_static_line("Carnage Tyrant can't be countered.").unwrap();
        assert_eq!(def.mode, StaticMode::CantBeCast);
        assert!(def.description.is_some());
    }

    #[test]
    fn static_spells_cost_less() {
        let def = parse_static_line("Spells you cast cost {1} less to cast.").unwrap();
        assert_eq!(def.mode, StaticMode::ReduceCost);
    }

    #[test]
    fn static_opponent_spells_cost_more() {
        let def = parse_static_line("Spells your opponents cast cost {1} more to cast.").unwrap();
        assert_eq!(def.mode, StaticMode::RaiseCost);
    }

    // NOTE: static_enters_with_counters test moved to oracle_replacement tests —
    // "enters with counters" is now parsed as a Moved replacement effect.

    #[test]
    fn static_as_long_as_chosen_color() {
        let def = parse_static_line(
            "As long as the chosen color is blue, enchanted creature has flying.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.condition,
            Some(StaticCondition::ChosenColorIs {
                color: crate::types::mana::ManaColor::Blue
            })
        ));
    }

    #[test]
    fn static_as_long_as_hand_size_gt_life() {
        use crate::types::ability::{Comparator, QuantityExpr, QuantityRef};
        let def = parse_static_line(
            "As long as the number of cards in your hand is greater than your life total, enchanted creature has trample.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.condition,
            Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize
                },
                comparator: Comparator::GT,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal
                },
            })
        ));
    }

    #[test]
    fn static_as_long_as_unrecognized_condition() {
        // Conditions the parser cannot yet decompose fall through to Unrecognized.
        // The whole "As long as X, Y" string is captured permissively so the effect still fires.
        let def = parse_static_line(
            "As long as you cast this spell from exile, enchanted creature gets +1/+1.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.condition,
            Some(StaticCondition::Unrecognized { .. })
        ));
    }

    #[test]
    fn static_has_keyword_as_long_as() {
        let def =
            parse_static_line("Tarmogoyf has trample as long as a land card is in a graveyard.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }));
        assert!(matches!(
            def.condition,
            Some(StaticCondition::Unrecognized { .. })
        ));
    }

    #[test]
    fn static_life_more_than_starting_conditional() {
        let def = parse_static_line(
            "As long as you have at least 7 life more than your starting life total, creatures you control get +2/+2.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Creature),
                controller: Some(ControllerRef::You),
                ..
            }))
        ));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 2 }));
        assert_eq!(
            def.condition,
            Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeAboveStarting
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 7 },
            })
        );
    }

    #[test]
    fn static_devotion_condition() {
        use crate::types::mana::ManaColor;
        let def = parse_static_line(
            "As long as your devotion to black is less than five, Erebos isn't a creature.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.condition,
            Some(StaticCondition::DevotionGE {
                colors: vec![ManaColor::Black],
                threshold: 5,
            })
        );
    }

    #[test]
    fn static_devotion_multicolor_condition() {
        use crate::types::mana::ManaColor;
        let def = parse_static_line(
            "As long as your devotion to white and black is less than seven, Athreos isn't a creature.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.condition,
            Some(StaticCondition::DevotionGE {
                colors: vec![ManaColor::White, ManaColor::Black],
                threshold: 7,
            })
        );
    }

    #[test]
    fn static_during_your_turn_condition() {
        let def =
            parse_static_line("As long as it's your turn, Triumphant Adventurer has first strike.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
    }

    #[test]
    fn static_control_presence_condition() {
        let def =
            parse_static_line("As long as you control a artifact, Toolcraft Exemplar gets +2/+1.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.condition,
            Some(StaticCondition::IsPresent { filter: Some(_) })
        ));
    }

    #[test]
    fn static_control_creature_with_power_ge() {
        // "creature with power 4 or greater" — digit form
        let def = parse_static_line(
            "As long as you control a creature with power 4 or greater, Inspiring Commander gets +1/+1.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.condition,
            Some(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(_))
            })
        ));
        // Modifications should include PT buff
        assert!(def
            .modifications
            .iter()
            .any(|m| matches!(m, ContinuousModification::AddPower { value: 1 })));
    }

    #[test]
    fn static_control_creature_with_power_ge_word() {
        // "creature with power four or greater" — English word form via parse_number
        let def = parse_static_line(
            "As long as you control a creature with power four or greater, Target gets +2/+0.",
        )
        .unwrap();
        assert!(matches!(
            def.condition,
            Some(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(_))
            })
        ));
    }

    #[test]
    fn static_control_creature_with_power_le() {
        // "creature with power 2 or less"
        let def = parse_static_line(
            "As long as you control a creature with power 2 or less, Target gets -1/-0.",
        )
        .unwrap();
        assert!(matches!(
            def.condition,
            Some(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(_))
            })
        ));
    }

    #[test]
    fn static_lands_you_control_have() {
        let def = parse_static_line("Lands you control have 'Forests'.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddSubtype {
                subtype: "Forests".to_string(),
            }));
    }

    #[test]
    fn static_cant_be_the_target() {
        let def = parse_static_line(
            "Sphinx of the Final Word can't be the target of spells or abilities your opponents control.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::CantBeTargeted);
    }

    #[test]
    fn static_cant_be_sacrificed() {
        let def = parse_static_line("Sigarda, Host of Herons can't be sacrificed.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def.description.is_some());
    }

    #[test]
    fn map_keyword_uses_fromstr() {
        // Test that map_keyword handles all standard keywords via FromStr
        assert_eq!(map_keyword("flying"), Some(Keyword::Flying));
        assert_eq!(map_keyword("first strike"), Some(Keyword::FirstStrike));
        assert_eq!(map_keyword("double strike"), Some(Keyword::DoubleStrike));
        assert_eq!(map_keyword("trample"), Some(Keyword::Trample));
        assert_eq!(map_keyword("deathtouch"), Some(Keyword::Deathtouch));
        assert_eq!(map_keyword("lifelink"), Some(Keyword::Lifelink));
        assert_eq!(map_keyword("vigilance"), Some(Keyword::Vigilance));
        assert_eq!(map_keyword("haste"), Some(Keyword::Haste));
        assert_eq!(map_keyword("reach"), Some(Keyword::Reach));
        assert_eq!(map_keyword("menace"), Some(Keyword::Menace));
        assert_eq!(map_keyword("hexproof"), Some(Keyword::Hexproof));
        assert_eq!(map_keyword("indestructible"), Some(Keyword::Indestructible));
        assert_eq!(map_keyword("defender"), Some(Keyword::Defender));
        assert_eq!(map_keyword("shroud"), Some(Keyword::Shroud));
        assert_eq!(map_keyword("flash"), Some(Keyword::Flash));
        assert_eq!(map_keyword("prowess"), Some(Keyword::Prowess));
        assert_eq!(map_keyword("fear"), Some(Keyword::Fear));
        assert_eq!(map_keyword("intimidate"), Some(Keyword::Intimidate));
        assert_eq!(map_keyword("wither"), Some(Keyword::Wither));
        assert_eq!(map_keyword("infect"), Some(Keyword::Infect));
        // Unknown returns None
        assert_eq!(map_keyword("notakeyword"), None);
    }

    #[test]
    fn static_multiple_keywords() {
        let def = parse_static_line("Enchanted creature has flying, trample, and haste.").unwrap();
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }));
    }

    #[test]
    fn static_self_gets_pt() {
        let def = parse_static_line("Tarmogoyf gets +1/+2.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 2 }));
    }

    #[test]
    fn static_have_keyword() {
        let def = parse_static_line("Creatures you control have vigilance.").unwrap();
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Vigilance,
            }));
    }

    #[test]
    fn during_your_turn_has_lifelink() {
        let def = parse_static_line("During your turn, this creature has lifelink.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Lifelink,
            }));
    }

    #[test]
    fn this_land_is_the_chosen_type() {
        let def = parse_static_line("This land is the chosen type.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::BasicLandType,
            }]
        );
    }

    #[test]
    fn this_creature_is_the_chosen_type() {
        let def =
            parse_static_line("This creature is the chosen type in addition to its other types.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::CreatureType,
            }]
        );
    }

    #[test]
    fn static_tarmogoyf_cda() {
        let def = parse_static_line(
            "Tarmogoyf's power is equal to the number of card types among cards in all graveyards and its toughness is equal to that number plus 1.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def.characteristic_defining);
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetDynamicPower {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::CardTypesInGraveyards {
                        scope: CountScope::All,
                    },
                },
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetDynamicToughness {
                value: QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::CardTypesInGraveyards {
                            scope: CountScope::All,
                        },
                    }),
                    offset: 1,
                },
            }));
    }

    #[test]
    fn static_enchanted_creature_doesnt_untap() {
        let def = parse_static_line(
            "Enchanted creature doesn't untap during its controller's untap step.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantUntap".to_string()));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
    }

    #[test]
    fn static_creatures_with_counters_dont_untap() {
        let def = parse_static_line(
            "Creatures with ice counters on them don't untap during their controllers' untap steps.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Other("CantUntap".to_string()));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter::creature().properties(
                vec![FilterProp::CountersGE {
                    counter_type: "ice".to_string(),
                    count: 1,
                },]
            )))
        );
    }

    #[test]
    fn static_this_creature_attacks_each_combat_if_able() {
        let def = parse_static_line("This creature attacks each combat if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustAttack);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_enchanted_creature_attacks_each_combat_if_able() {
        let def = parse_static_line("Enchanted creature attacks each combat if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustAttack);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
    }

    #[test]
    fn static_this_creature_can_block_only_creatures_with_flying() {
        let def = parse_static_line("This creature can block only creatures with flying.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("BlockRestriction".to_string()));
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_you_have_shroud() {
        let def = parse_static_line("You have shroud.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("Shroud".to_string()));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
    }

    #[test]
    fn static_you_have_no_maximum_hand_size() {
        let def = parse_static_line("You have no maximum hand size.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("NoMaximumHandSize".to_string()));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
    }

    #[test]
    fn static_each_player_may_play_an_additional_land() {
        let def =
            parse_static_line("Each player may play an additional land on each of their turns.")
                .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::Other("MayPlayAdditionalLand".to_string())
        );
        assert_eq!(def.affected, Some(TargetFilter::Player));
    }

    #[test]
    fn static_you_may_choose_not_to_untap_self() {
        let def =
            parse_static_line("You may choose not to untap this creature during your untap step.")
                .unwrap();
        assert_eq!(
            def.mode,
            StaticMode::Other("MayChooseNotToUntap".to_string())
        );
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_you_may_look_at_top_card_of_library() {
        let def =
            parse_static_line("You may look at the top card of your library any time.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::Other("MayLookAtTopOfLibrary".to_string())
        );
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
        );
    }

    #[test]
    fn static_cards_in_graveyards_lose_all_abilities() {
        let def = parse_static_line("Cards in graveyards lose all abilities.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::InZone {
                    zone: crate::types::zones::Zone::Graveyard,
                },
            ])))
        );
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::RemoveAllAbilities]
        );
    }

    #[test]
    fn static_black_creatures_get_plus_one_plus_one() {
        let def = parse_static_line("Black creatures get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter::creature().properties(
                vec![FilterProp::HasColor {
                    color: "Black".to_string(),
                },]
            )))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
    }

    #[test]
    fn static_creatures_you_control_with_mana_value_filter() {
        let def = parse_static_line("Creatures you control with mana value 3 or less get +1/+0.")
            .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::CmcLE { value: 3 }]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 0 }));
    }

    #[test]
    fn static_creatures_you_control_with_flying_filter() {
        let def = parse_static_line("Creatures you control with flying get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::WithKeyword {
                        value: "flying".to_string(),
                    }]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
    }

    #[test]
    fn static_other_zombie_creatures_have_swampwalk() {
        let def = parse_static_line("Other Zombie creatures have swampwalk.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .subtype("Zombie".to_string())
                    .properties(vec![FilterProp::Another]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Landwalk("Swamp".to_string()),
            }));
    }

    #[test]
    fn static_creature_tokens_you_control_lose_all_abilities_and_have_base_pt() {
        let def = parse_static_line(
            "Creature tokens you control lose all abilities and have base power and toughness 3/3.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Token]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::RemoveAllAbilities));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetPower { value: 3 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetToughness { value: 3 }));
    }

    #[test]
    fn static_target_subject_can_set_base_power_without_toughness() {
        let modifications = parse_continuous_modifications("has base power 3 until end of turn");
        assert_eq!(
            modifications,
            vec![ContinuousModification::SetPower { value: 3 }]
        );
    }

    #[test]
    fn static_enchanted_land_has_quoted_ability() {
        let def = parse_static_line("Enchanted land has \"{T}: Add two mana of any one color.\"")
            .unwrap();
        // Should produce a GrantAbility with a typed activated AbilityDefinition
        let grant = def
            .modifications
            .iter()
            .find(|m| matches!(m, ContinuousModification::GrantAbility { .. }));
        assert!(
            grant.is_some(),
            "should contain a GrantAbility modification"
        );
        if let ContinuousModification::GrantAbility { definition } = grant.unwrap() {
            assert_eq!(definition.kind, AbilityKind::Activated);
            assert!(definition.cost.is_some());
        }
    }

    #[test]
    fn static_other_tapped_creatures_you_control_have_indestructible() {
        let def =
            parse_static_line("Other tapped creatures you control have indestructible.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Tapped, FilterProp::Another]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Indestructible,
            }));
    }

    #[test]
    fn static_attacking_creatures_you_control_have_double_strike() {
        let def = parse_static_line("Attacking creatures you control have double strike.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Attacking]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::DoubleStrike,
            }));
    }

    #[test]
    fn static_during_your_turn_creatures_you_control_have_hexproof() {
        let def =
            parse_static_line("During your turn, creatures you control have hexproof.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }));
    }

    #[test]
    fn static_during_your_turn_equipped_creatures_you_control_have_double_strike() {
        let def = parse_static_line(
            "During your turn, equipped creatures you control have double strike and haste.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(def.condition, Some(StaticCondition::DuringYourTurn));
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::EquippedBy]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::DoubleStrike,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }));
    }

    #[test]
    fn parse_compound_static_kaito_animation() {
        let text = "During your turn, as long as ~ has one or more loyalty counters on him, he's a 3/4 Ninja creature and has hexproof.";
        let def = parse_static_line(text).unwrap();

        // Verify compound condition
        assert!(matches!(
            def.condition,
            Some(StaticCondition::And { ref conditions })
            if conditions.len() == 2
        ));
        if let Some(StaticCondition::And { ref conditions }) = def.condition {
            assert!(matches!(conditions[0], StaticCondition::DuringYourTurn));
            assert!(matches!(
                conditions[1],
                StaticCondition::HasCounters {
                    ref counter_type,
                    minimum: 1,
                } if counter_type == "loyalty"
            ));
        }

        // Verify self-referencing
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));

        // Verify modifications
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetPower { value: 3 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetToughness { value: 4 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Creature,
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddSubtype {
                subtype: "Ninja".to_string(),
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Hexproof,
            }));
    }

    // ── New static routing tests (Steps 4-5) ─────────────────────────────

    #[test]
    fn static_must_be_blocked_if_able() {
        // CR 509.1b: "must be blocked if able"
        let def = parse_static_line("Darksteel Myr must be blocked if able.").unwrap();
        assert_eq!(def.mode, StaticMode::Other("MustBeBlocked".to_string()));
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_opponents_cant_gain_life() {
        // CR 119.7: Lifegain prevention — opponent scope
        let def = parse_static_line("Your opponents can't gain life.").unwrap();
        assert_eq!(def.mode, StaticMode::CantGainLife);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::Opponent),
                ..
            }))
        ));
    }

    #[test]
    fn static_you_cant_gain_life() {
        // CR 119.7: Lifegain prevention — self scope
        let def = parse_static_line("You can't gain life.").unwrap();
        assert_eq!(def.mode, StaticMode::CantGainLife);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                ..
            }))
        ));
    }

    #[test]
    fn static_players_cant_gain_life() {
        // CR 119.7: Lifegain prevention — all players
        let def = parse_static_line("Players can't gain life.").unwrap();
        assert_eq!(def.mode, StaticMode::CantGainLife);
        // No controller restriction — affects all
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(TypedFilter {
                controller: None,
                ..
            }))
        ));
    }

    #[test]
    fn static_cast_as_though_flash() {
        // CR 702.8d: Flash-granting static
        let def =
            parse_static_line("You may cast creature spells as though they had flash.").unwrap();
        assert_eq!(def.mode, StaticMode::CastWithFlash);
    }

    #[test]
    fn static_can_block_additional_creature() {
        let def = parse_static_line("Palace Guard can block an additional creature each combat.")
            .unwrap();
        assert_eq!(def.mode, StaticMode::ExtraBlockers { count: Some(1) });
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_can_block_any_number() {
        let def =
            parse_static_line("Hundred-Handed One can block any number of creatures.").unwrap();
        assert_eq!(def.mode, StaticMode::ExtraBlockers { count: None });
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_play_two_additional_lands() {
        // "play two additional lands" — not handled by the subject-predicate parser
        let def =
            parse_static_line("You may play two additional lands on each of your turns.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::Other("AdditionalLandDrop".to_string())
        );
    }

    #[test]
    fn parse_compound_static_counter_minimum_variants() {
        // "a" counter variant
        let text =
            "During your turn, as long as ~ has a loyalty counter on it, it's a 2/2 Ninja creature and has hexproof.";
        let def = parse_static_line(text).unwrap();
        if let Some(StaticCondition::And { ref conditions }) = def.condition {
            assert!(matches!(
                conditions[1],
                StaticCondition::HasCounters { minimum: 1, .. }
            ));
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetPower { value: 2 }));
    }

    // ── CR 510.1c: AssignDamageFromToughness (Doran-class) ─────────────

    #[test]
    fn static_assigns_damage_from_toughness_basic() {
        // CR 510.1c: "Each creature you control assigns combat damage equal to its toughness"
        let def = parse_static_line(
            "Each creature you control assigns combat damage equal to its toughness rather than its power.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AssignDamageFromToughness));
    }

    #[test]
    fn static_assigns_damage_from_toughness_with_defender() {
        // CR 510.1c: "Each creature you control with defender assigns combat damage..."
        let def = parse_static_line(
            "Each creature you control with defender assigns combat damage equal to its toughness rather than its power.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::WithKeyword {
                        value: "defender".to_string(),
                    }]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AssignDamageFromToughness));
    }

    #[test]
    fn static_assigns_damage_from_toughness_gt_power() {
        // CR 510.1c: "Each creature you control with toughness greater than its power..."
        let def = parse_static_line(
            "Each creature you control with toughness greater than its power assigns combat damage equal to its toughness rather than its power.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::ToughnessGTPower]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AssignDamageFromToughness));
    }

    // --- Conditional counter-based keyword grants (CR 613.7) ---

    #[test]
    fn static_each_creature_with_counter_has_trample() {
        let def =
            parse_static_line("Each creature you control with a +1/+1 counter on it has trample.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Creature),
                controller: Some(ControllerRef::You),
                properties,
                ..
            })) => {
                assert!(properties.iter().any(|p| matches!(
                    p,
                    FilterProp::CountersGE {
                        ref counter_type,
                        count: 1,
                    } if counter_type == "+1/+1"
                )));
            }
            other => panic!("Expected Typed creature filter, got {:?}", other),
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample
            }));
    }

    #[test]
    fn static_creatures_with_counters_have_haste() {
        let def =
            parse_static_line("Creatures you control with +1/+1 counters on them have haste.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(TypedFilter {
                card_type: Some(TypeFilter::Creature),
                controller: Some(ControllerRef::You),
                properties,
                ..
            })) => {
                assert!(properties.iter().any(|p| matches!(
                    p,
                    FilterProp::CountersGE {
                        ref counter_type,
                        count: 1,
                    } if counter_type == "+1/+1"
                )));
            }
            other => panic!("Expected Typed creature filter, got {:?}", other),
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste
            }));
    }

    #[test]
    fn static_creatures_with_counter_get_pump() {
        let def = parse_static_line("Creatures you control with a +1/+1 counter on it gets +2/+2.")
            .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        match &def.affected {
            Some(TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::You),
                properties,
                ..
            })) => {
                assert!(properties.iter().any(|p| matches!(
                    p,
                    FilterProp::CountersGE {
                        ref counter_type,
                        count: 1,
                    } if counter_type == "+1/+1"
                )));
            }
            other => panic!("Expected Typed creature filter, got {:?}", other),
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }));
    }

    #[test]
    fn parse_quantity_ref_object_count() {
        let qty = parse_quantity_ref("the number of creatures you control").unwrap();
        assert!(
            matches!(qty, QuantityRef::ObjectCount { .. }),
            "Expected ObjectCount, got {qty:?}"
        );
    }

    #[test]
    fn parse_quantity_ref_subtype_count() {
        let qty = parse_quantity_ref("the number of Allies you control").unwrap();
        assert!(
            matches!(qty, QuantityRef::ObjectCount { .. }),
            "Expected ObjectCount, got {qty:?}"
        );
    }

    #[test]
    fn parse_quantity_ref_devotion_single() {
        let qty = parse_quantity_ref("your devotion to black").unwrap();
        match qty {
            QuantityRef::Devotion { colors } => {
                assert_eq!(colors, vec![crate::types::mana::ManaColor::Black]);
            }
            other => panic!("Expected Devotion, got {other:?}"),
        }
    }

    #[test]
    fn parse_quantity_ref_devotion_multi() {
        let qty = parse_quantity_ref("your devotion to black and red").unwrap();
        match qty {
            QuantityRef::Devotion { colors } => {
                assert_eq!(colors.len(), 2);
                assert!(colors.contains(&crate::types::mana::ManaColor::Black));
                assert!(colors.contains(&crate::types::mana::ManaColor::Red));
            }
            other => panic!("Expected Devotion, got {other:?}"),
        }
    }

    // --- split_keyword_list protection-awareness tests ---

    /// Helper: collect split results as owned strings for easy comparison.
    fn kw_list(text: &str) -> Vec<String> {
        split_keyword_list(text)
            .into_iter()
            .map(|c| c.into_owned())
            .collect()
    }

    #[test]
    fn split_keyword_list_two_color_protections() {
        assert_eq!(
            kw_list("protection from black and from red"),
            vec!["protection from black", "protection from red"]
        );
    }

    #[test]
    fn split_keyword_list_non_protection_and() {
        assert_eq!(
            kw_list("flying and first strike"),
            vec!["flying", "first strike"]
        );
    }

    #[test]
    fn split_keyword_list_mixed_keywords_and_protection() {
        // expand_protection_parts lowercases protection fragments
        assert_eq!(
            kw_list("flying, protection from Demons and from Dragons, and first strike"),
            vec![
                "flying",
                "protection from demons",
                "protection from dragons",
                "first strike"
            ]
        );
    }

    #[test]
    fn split_keyword_list_three_way_inline_protection() {
        assert_eq!(
            kw_list("protection from red and from blue and from green"),
            vec![
                "protection from red",
                "protection from blue",
                "protection from green"
            ]
        );
    }

    #[test]
    fn split_keyword_list_comma_continuation_protection() {
        // expand_protection_parts lowercases protection fragments
        assert_eq!(
            kw_list("protection from Vampires, from Werewolves, and from Zombies"),
            vec![
                "protection from vampires",
                "protection from werewolves",
                "protection from zombies"
            ]
        );
    }

    #[test]
    fn split_keyword_list_protection_from_everything_no_split() {
        assert_eq!(
            kw_list("protection from everything"),
            vec!["protection from everything"]
        );
    }

    #[test]
    fn continuous_mods_protection_from_two_colors() {
        use crate::types::keywords::ProtectionTarget;
        let mods = parse_continuous_modifications("has protection from black and from red");
        let prot_keywords: Vec<_> = mods
            .iter()
            .filter_map(|m| match m {
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Protection(pt),
                } => Some(pt.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            prot_keywords,
            vec![
                ProtectionTarget::Color(ManaColor::Black),
                ProtectionTarget::Color(ManaColor::Red),
            ]
        );
    }

    // ── parse_cda_quantity tests ────────────────────────────────────────

    #[test]
    fn cda_quantity_self_power() {
        let qty = parse_cda_quantity("~'s power").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::SelfPower
            }
        ));
    }

    #[test]
    fn cda_quantity_self_toughness() {
        let qty = parse_cda_quantity("this creature's toughness").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::SelfToughness
            }
        ));
    }

    #[test]
    fn cda_quantity_opponents() {
        let qty = parse_cda_quantity("the number of opponents you have").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::Opponent
                }
            }
        ));
    }

    #[test]
    fn cda_quantity_counters_on_self() {
        let qty = parse_cda_quantity("the number of +1/+1 counters on ~").unwrap();
        match qty {
            QuantityExpr::Ref {
                qty: QuantityRef::CountersOnSelf { counter_type },
            } => assert_eq!(counter_type, "plus1/plus1"),
            other => panic!("Expected CountersOnSelf, got {other:?}"),
        }
    }

    #[test]
    fn cda_quantity_greatest_power() {
        let qty = parse_cda_quantity("the greatest power among creatures you control").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Max,
                    property: ObjectProperty::Power,
                    ..
                }
            }
        ));
    }

    #[test]
    fn cda_quantity_greatest_toughness() {
        let qty = parse_cda_quantity("the greatest toughness among creatures you control").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Max,
                    property: ObjectProperty::Toughness,
                    ..
                }
            }
        ));
    }

    #[test]
    fn cda_quantity_greatest_mana_value() {
        let qty =
            parse_cda_quantity("the greatest mana value among creatures you control").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Max,
                    property: ObjectProperty::ManaValue,
                    ..
                }
            }
        ));
    }

    #[test]
    fn cda_quantity_total_power() {
        let qty = parse_cda_quantity("the total power of creatures you control").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Sum,
                    property: ObjectProperty::Power,
                    ..
                }
            }
        ));
    }

    #[test]
    fn cda_quantity_twice() {
        let qty = parse_cda_quantity("twice the number of creatures you control").unwrap();
        match qty {
            QuantityExpr::Multiply { factor, inner } => {
                assert_eq!(factor, 2);
                assert!(matches!(
                    *inner,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. }
                    }
                ));
            }
            other => panic!("Expected Multiply, got {other:?}"),
        }
    }

    #[test]
    fn cda_quantity_n_plus_inner() {
        let qty = parse_cda_quantity("1 plus the number of creatures you control").unwrap();
        match qty {
            QuantityExpr::Offset { inner, offset } => {
                assert_eq!(offset, 1);
                assert!(matches!(
                    *inner,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. }
                    }
                ));
            }
            other => panic!("Expected Offset, got {other:?}"),
        }
    }
}
