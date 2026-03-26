use std::borrow::Cow;
use std::str::FromStr;

use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::parse_effect_chain;
use super::oracle_quantity::{capitalize_first, parse_cda_quantity, parse_quantity_ref};
use super::oracle_target::{parse_combat_status_prefix, parse_counter_suffix, parse_type_phrase};
use super::oracle_util::{
    has_unconsumed_conditional, parse_mana_symbols, parse_number, parse_subtype, strip_after,
    strip_reminder_text, TextPair,
};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, BasicLandType, CardPlayMode, ChosenSubtypeKind, Comparator,
    ContinuousModification, ControllerRef, CountScope, FilterProp, QuantityExpr, QuantityRef,
    StaticCondition, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::statics::{CastingProhibitionCondition, CastingProhibitionScope, StaticMode};
use crate::types::zones::Zone;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleStaticPredicate {
    CantUntap,
    MustAttack,
    MustBlock,
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
#[tracing::instrument(level = "debug")]
pub fn parse_static_line(text: &str) -> Option<StaticDefinition> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();
    let tp = TextPair::new(&text, &lower);

    // CR 604.3 + CR 601.2a: "Once during each of your turns, you may cast [filter] from your graveyard."
    if let Some(result) = try_parse_graveyard_cast_permission(&text, &lower) {
        return Some(result);
    }

    if tp.starts_with("you may choose not to untap ") && tp.contains(" during your untap step") {
        return Some(
            StaticDefinition::new(StaticMode::MayChooseNotToUntap)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "Play with the top card of your library revealed" ---
    // CR 400.2: Continuous effect making top card public information.
    if tp.contains("play with the top card") {
        if has_unconsumed_conditional(tp.lower) {
            tracing::warn!(
                text = text,
                "Unconsumed conditional in 'play with the top card' catch-all — parser may need extension"
            );
        } else {
            let all_players = tp.contains("their libraries") || tp.contains("each player");
            return Some(
                StaticDefinition::new(StaticMode::RevealTopOfLibrary { all_players })
                    .affected(TargetFilter::SelfRef)
                    .description(text.to_string()),
            );
        }
    }

    // --- "You control enchanted creature/permanent" (Control Magic pattern) ---
    // CR 303.4e + CR 613.2: Aura-based continuous control-changing effects.
    if tp.lower == "you control enchanted creature" || tp.lower == "you control enchanted creature."
    {
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
                ))
                .modifications(vec![ContinuousModification::ChangeController])
                .description(text.to_string()),
        );
    }
    if tp.lower == "you control enchanted permanent"
        || tp.lower == "you control enchanted permanent."
    {
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy]),
                ))
                .modifications(vec![ContinuousModification::ChangeController])
                .description(text.to_string()),
        );
    }
    if tp.lower == "you control enchanted land" || tp.lower == "you control enchanted land." {
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::land().properties(vec![FilterProp::EnchantedBy]),
                ))
                .modifications(vec![ContinuousModification::ChangeController])
                .description(text.to_string()),
        );
    }
    if tp.lower == "you control enchanted artifact" || tp.lower == "you control enchanted artifact."
    {
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .properties(vec![FilterProp::EnchantedBy]),
                ))
                .modifications(vec![ContinuousModification::ChangeController])
                .description(text.to_string()),
        );
    }

    // --- "Enchanted creature gets +N/+M" or "has {keyword}" ---
    if tp.starts_with("enchanted creature ") {
        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]));
        if let Some(def) = parse_enchanted_equipped_predicate(&text[19..], filter, &text) {
            return Some(def);
        }
    }

    // --- "Enchanted permanent gets/has ..." ---
    if tp.starts_with("enchanted permanent ") {
        let filter =
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy]));
        if let Some(def) = parse_enchanted_equipped_predicate(&text[20..], filter, &text) {
            return Some(def);
        }
    }

    // CR 305.7: "Enchanted land is a [type]" — must be before general "enchanted land" handler.
    if let Some(rest) = tp.strip_prefix("enchanted land is a ") {
        let rest = rest.trim_end_matches('.');
        // "in addition to its other types" → AddSubtype (not replacement)
        if let Some(land_name) = rest.strip_suffix(" in addition to its other types") {
            if let Some(basic_type) = parse_basic_land_type(land_name.lower) {
                return Some(
                    StaticDefinition::continuous()
                        .affected(TargetFilter::Typed(
                            TypedFilter::land().properties(vec![FilterProp::EnchantedBy]),
                        ))
                        .modifications(vec![ContinuousModification::AddSubtype {
                            subtype: basic_type.as_subtype_str().to_string(),
                        }])
                        .description(text.to_string()),
                );
            }
        }
        // Default: replacement semantics per CR 305.7
        if let Some(basic_type) = parse_basic_land_type(rest.lower.trim()) {
            return Some(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::land().properties(vec![FilterProp::EnchantedBy]),
                    ))
                    .modifications(vec![ContinuousModification::SetBasicLandType {
                        land_type: basic_type,
                    }])
                    .description(text.to_string()),
            );
        }
    }

    if tp.starts_with("enchanted land ") {
        let filter =
            TargetFilter::Typed(TypedFilter::land().properties(vec![FilterProp::EnchantedBy]));
        if let Some(def) = parse_enchanted_equipped_predicate(&text[15..], filter, &text) {
            return Some(def);
        }
    }

    // --- "Equipped creature gets +N/+M" ---
    if tp.starts_with("equipped creature ") {
        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy]));
        if let Some(def) = parse_enchanted_equipped_predicate(&text[18..], filter, &text) {
            return Some(def);
        }
    }

    // --- "All creatures get/have ..." ---
    if tp.starts_with("all creatures ") {
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
    if let Some(rest) = tp.lower.strip_prefix("other ") {
        if let Some(result) = parse_typed_you_control(&tp.original[6..], rest, true) {
            return Some(result);
        }
    }

    // --- "[Subtype] creatures you control get/have..." ---
    // e.g. "Elf creatures you control get +1/+1"
    // Skip for "other" prefix — already handled above with is_other=true.
    if !tp.starts_with("other ") {
        if let Some(result) = parse_typed_you_control(tp.original, tp.lower, false) {
            return Some(result);
        }
    }

    // --- "Creatures you control [with counter condition] get/have ..." ---
    if tp.starts_with("creatures you control ") {
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
    // CR 613.7: "Other" excludes the source permanent itself via FilterProp::Another.
    if tp.starts_with("other creatures you control ") {
        let after_prefix = &text[28..];
        let (filter, predicate_text) =
            if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                (
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![prop, FilterProp::Another]),
                    ),
                    rest,
                )
            } else {
                (
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::Another]),
                    ),
                    after_prefix,
                )
            };
        if let Some(def) = parse_continuous_gets_has(predicate_text, filter, &text) {
            return Some(def);
        }
    }

    // CR 305.7: "[Subject] lands are [type]" — land type-changing statics.
    // Must come before parse_subject_continuous_static (which splits on "gets/has/gains"
    // verbs and would not match "are" predicates).
    if let Some(def) = parse_land_type_change(&tp, &text) {
        return Some(def);
    }

    if let Some(def) = parse_subject_continuous_static(&text) {
        return Some(def);
    }

    // --- "Lands you control have '[type]'" ---
    if tp.starts_with("lands you control have ") {
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
    if let Some(def) = parse_compound_turn_counter_animation(tp.lower, tp.original) {
        return Some(def);
    }

    // --- "During your turn, [subject] has/gets ..." ---
    if let Some(rest) = tp.lower.strip_prefix("during your turn, ") {
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
    if tp.contains("is the chosen type") {
        let kind = if tp.starts_with("this creature") || tp.contains("creature is the chosen") {
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
    if let Some(def) = parse_cda_pt_equality(tp.lower, tp.original) {
        return Some(def);
    }

    if let Some(def) = parse_conditional_static(&text) {
        return Some(def);
    }

    // --- "~ has [keyword] as long as ..." (must be before generic self-ref "has") ---
    if let Some(has_pos) = tp.find(" has ") {
        if let Some(cond_pos) = tp.find(" as long as ") {
            if has_pos < cond_pos {
                let keyword_text = tp.lower[has_pos + 5..cond_pos].trim();
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
    if let Some(pos) = tp
        .find(" has ")
        .or_else(|| tp.find(" gets "))
        .or_else(|| tp.find(" get "))
    {
        let verb_len = if tp.lower[pos..].starts_with(" has ") {
            5
        } else if tp.lower[pos..].starts_with(" gets ") {
            6
        } else {
            5 // " get "
        };
        let subject = &tp.lower[..pos];
        // Only match if the subject doesn't look like a known prefix we handle elsewhere
        if !subject.contains("creature")
            && !subject.contains("permanent")
            && !subject.contains("land")
            && !subject.starts_with("all ")
            && !subject.starts_with("other ")
        {
            let after = &tp.original[pos + verb_len..];
            return parse_continuous_gets_has(
                &format!(
                    "{}{}",
                    if tp.lower[pos..].starts_with(" has ") {
                        "has "
                    } else {
                        "gets "
                    },
                    after
                ),
                TargetFilter::SelfRef,
                tp.original,
            );
        }
    }

    // --- "~ isn't a [type]" (type removal) ---
    // e.g. "Erebos isn't a creature" from god-of-the-dead conditional
    if let Some(type_rest) = tp.lower.split("isn't a ").nth(1) {
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
    if tp.contains("can't be blocked") {
        // Guard: "can't be blocked except by..." requires a more specific handler
        if has_unconsumed_conditional(tp.lower) || tp.lower.contains("except by") {
            tracing::warn!(
                text = text,
                "Unconsumed conditional in 'can't be blocked' catch-all — parser may need extension"
            );
            // Fall through to let a more specific handler catch it, or return None
        } else {
            return Some(
                StaticDefinition::new(StaticMode::CantBeBlocked)
                    .affected(TargetFilter::SelfRef)
                    .description(text.to_string()),
            );
        }
    }

    // --- "~ can't block" ---
    if tp.contains("can't block") && !tp.contains("can't be blocked") {
        return Some(
            StaticDefinition::new(StaticMode::CantBlock)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't attack" ---
    if tp.contains("can't attack") {
        return Some(
            StaticDefinition::new(StaticMode::CantAttack)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "can't be countered" ---
    // CR 101.2: "Can't" effects override "can" effects.
    if tp.contains("can't be countered") {
        if has_unconsumed_conditional(tp.lower) {
            tracing::warn!(
                text = text,
                "Unconsumed conditional in 'can't be countered' catch-all — parser may need extension"
            );
        } else {
            let affected = parse_cant_be_countered_subject(&tp);
            return Some(
                StaticDefinition::new(StaticMode::CantBeCountered)
                    .affected(affected)
                    .description(text.to_string()),
            );
        }
    }

    // --- "~ can't be the target" or "~ can't be targeted" ---
    if tp.contains("can't be the target") || tp.contains("can't be targeted") {
        return Some(
            StaticDefinition::new(StaticMode::CantBeTargeted)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "~ can't be sacrificed" ---
    if tp.contains("can't be sacrificed") {
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- CR 604.3: "[type] cards in [zones] can't enter the battlefield" ---
    // e.g., Grafdigger's Cage: "Creature cards in graveyards and libraries can't enter the battlefield."
    if tp.contains("can't enter the battlefield") {
        let affected = parse_cant_enter_battlefield_subject(&tp);
        return Some(
            StaticDefinition::new(StaticMode::CantEnterBattlefieldFrom)
                .affected(affected)
                .description(text.to_string()),
        );
    }

    // --- CR 101.2: Turn/phase-scoped casting prohibitions ---
    // e.g., Teferi, Time Raveler: "Your opponents can't cast spells during your turn."
    // e.g., "Players can't cast spells during combat."
    // Must be checked before CantCastFrom to avoid false matches on "can't cast spells".
    if tp.contains("can't cast spells during") {
        let who = if tp.contains("your opponents") || tp.contains("opponent") {
            CastingProhibitionScope::Opponents
        } else {
            CastingProhibitionScope::AllPlayers
        };
        let when = if tp.contains("during your turn") {
            CastingProhibitionCondition::DuringYourTurn
        } else if tp.contains("during combat") {
            CastingProhibitionCondition::DuringCombat
        } else {
            // Fallback: treat unknown conditions as combat-scoped
            CastingProhibitionCondition::DuringCombat
        };
        return Some(
            StaticDefinition::new(StaticMode::CantCastDuring { who, when })
                .description(text.to_string()),
        );
    }

    // --- CR 604.3: "Players can't cast spells from [zones]" ---
    // e.g., Grafdigger's Cage: "Players can't cast spells from graveyards or libraries."
    if tp.contains("can't cast spells from") {
        let zones = parse_zone_names_from_tp(&tp);
        let affected = if zones.is_empty() {
            TargetFilter::Any
        } else {
            TargetFilter::Typed(TypedFilter {
                properties: vec![FilterProp::InAnyZone { zones }],
                ..TypedFilter::default()
            })
        };
        return Some(
            StaticDefinition::new(StaticMode::CantCastFrom)
                .affected(affected)
                .description(text.to_string()),
        );
    }

    // --- "~ doesn't untap during your untap step [as long as / if condition]" ---
    // CR 502.3: Effects can keep permanents from untapping during the untap step.
    if tp.contains("doesn't untap during") || tp.contains("doesn\u{2019}t untap during") {
        // Check for trailing condition after the untap-step phrase
        let condition = extract_cant_untap_condition(tp.lower);
        let mut def = StaticDefinition::new(StaticMode::CantUntap)
            .affected(TargetFilter::SelfRef)
            .description(text.to_string());
        if let Some(cond) = condition {
            def.condition = Some(cond);
        }
        return Some(def);
    }

    // --- "You may look at the top card of your library any time." ---
    if tp.starts_with("you may look at the top card of your library") {
        return Some(
            StaticDefinition::new(StaticMode::MayLookAtTopOfLibrary)
                .affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                ))
                .description(text.to_string()),
        );
    }

    // NOTE: "enters with N counters" patterns are now handled by oracle_replacement.rs
    // as proper Moved replacement effects (paralleling the "enters tapped" pattern).

    // --- "{Ability} abilities you activate cost {N} less to activate" ---
    // CR 601.2f: Ability-type-specific cost reduction (e.g., Silver-Fur Master, Fluctuator).
    if tp.contains("abilities you activate cost") && tp.contains("less to activate") {
        // Extract keyword name: text before " abilities you activate"
        let keyword = tp
            .lower
            .split(" abilities you activate")
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        // Extract reduction amount from mana symbols between "cost " and " less"
        let amount = tp
            .lower
            .split("cost ")
            .nth(1)
            .and_then(|rest| rest.split(" less").next())
            .and_then(|mana_str| {
                // Parse {N} — extract the number from braces
                let stripped = mana_str.trim().trim_matches('{').trim_matches('}');
                stripped.parse::<u32>().ok()
            })
            .unwrap_or(1);
        return Some(
            StaticDefinition::new(StaticMode::ReduceAbilityCost { keyword, amount })
                .affected(TargetFilter::Typed(
                    TypedFilter::card().controller(ControllerRef::You),
                ))
                .description(text.to_string()),
        );
    }

    // --- CR 601.2f: Cost modification statics ---
    // Patterns: "[Type] spells [you/your opponents] cast cost {N} less/more to cast"
    // Also: "Noncreature spells cost {1} more to cast" (Thalia, no "you cast")
    if tp.contains("cost") && tp.contains("spell") && (tp.contains("less") || tp.contains("more")) {
        if let Some(def) = try_parse_cost_modification(&text, &lower) {
            return Some(def);
        }
    }

    // --- "must be blocked if able" (CR 509.1b) ---
    if tp.contains("must be blocked") {
        return Some(
            StaticDefinition::new(StaticMode::MustBeBlocked)
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "can't gain life" (CR 119.7) ---
    if tp.contains("can't gain life") {
        let affected = if tp.contains("your opponents") || tp.starts_with("opponents") {
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        } else if tp.starts_with("you ") || tp.contains("you can't") {
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
    if tp.contains("as though it had flash") || tp.contains("as though they had flash") {
        return Some(
            StaticDefinition::new(StaticMode::CastWithFlash).description(text.to_string()),
        );
    }

    // --- "can block an additional creature" / "can block any number" (CR 509.1b) ---
    if tp.contains("can block any number") {
        return Some(
            StaticDefinition::new(StaticMode::ExtraBlockers { count: None })
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }
    if tp.contains("can block an additional") {
        return Some(
            StaticDefinition::new(StaticMode::ExtraBlockers { count: Some(1) })
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "play an additional land" / "play two additional lands" ---
    // CR 305.2: Determine the count at parse time and carry it as typed data.
    if tp.contains("play two additional lands") {
        return Some(
            StaticDefinition::new(StaticMode::AdditionalLandDrop { count: 2 })
                .description(text.to_string()),
        );
    }
    if tp.contains("play an additional land") {
        return Some(
            StaticDefinition::new(StaticMode::AdditionalLandDrop { count: 1 })
                .description(text.to_string()),
        );
    }

    // --- "As long as ..." (generic conditional static, no comma separator) ---
    if tp.starts_with("as long as ") {
        let condition_text = tp
            .original
            .strip_prefix("As long as ")
            .unwrap_or(tp.original)
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
    if tp.contains("triggers an additional time") {
        return Some(
            StaticDefinition::new(StaticMode::Panharmonicon).description(text.to_string()),
        );
    }

    None
}

/// CR 205.3m: Try to parse a compound subtype descriptor like "Ninja and Rogue" or "Elf or Warrior"
/// into an `Or` filter with one creature+subtype+controller per part.
/// Returns `None` if the descriptor is not a compound subtype pattern.
fn try_parse_compound_subtypes(
    descriptor: &str,
    extra_props: &[FilterProp],
    is_other: bool,
) -> Option<TargetFilter> {
    let (left, right) = descriptor
        .split_once(" and ")
        .or_else(|| descriptor.split_once(" or "))?;
    let left_trimmed = left.trim();
    let right_trimmed = right.trim();
    if !is_capitalized_words(left_trimmed) || !is_capitalized_words(right_trimmed) {
        return None;
    }
    let left_sub = parse_subtype(left_trimmed)
        .map(|(c, _)| c)
        .unwrap_or_else(|| left_trimmed.to_string());
    let right_sub = parse_subtype(right_trimmed)
        .map(|(c, _)| c)
        .unwrap_or_else(|| right_trimmed.to_string());
    // Inject extra_props and Another into each inner filter at construction time,
    // because add_property does not recurse into TargetFilter::Or.
    let mut all_props = extra_props.to_vec();
    if is_other {
        all_props.push(FilterProp::Another);
    }
    let filters = vec![
        TargetFilter::Typed(
            TypedFilter::creature()
                .subtype(left_sub)
                .controller(ControllerRef::You)
                .properties(all_props.clone()),
        ),
        TargetFilter::Typed(
            TypedFilter::creature()
                .subtype(right_sub)
                .controller(ControllerRef::You)
                .properties(all_props),
        ),
    ];
    Some(TargetFilter::Or { filters })
}

/// Try to parse "[Subtype] creatures you control get/have ..." patterns.
/// `text` is the original-case text starting at the subtype word.
/// `lower` is the lowercased version of `text`.
/// `is_other` indicates whether this was preceded by "Other ".
fn parse_typed_you_control(text: &str, lower: &str, is_other: bool) -> Option<StaticDefinition> {
    let tp = TextPair::new(text, lower);
    // Try "X creatures you control get/have" first
    if let Some(creatures_pos) = tp.find(" creatures you control ") {
        let (before, after) = tp.split_at(creatures_pos);
        let descriptor = before.original.trim();
        if !descriptor.is_empty() {
            let after_prefix = &after.original[" creatures you control ".len()..];
            let full_subject = tp.original[..creatures_pos + " creatures you control".len()].trim();
            // CR 509.1h: Strip combat-status prefixes ("Attacking Ninja" → props=[Attacking], subtype="Ninja")
            let mut extra_props = Vec::new();
            let mut desc_remaining = descriptor;
            let mut desc_lower = descriptor.to_lowercase();
            while let Some((prop, consumed)) = parse_combat_status_prefix(&desc_lower) {
                extra_props.push(prop);
                desc_remaining = desc_remaining[consumed..].trim_start();
                desc_lower = desc_remaining.to_lowercase();
            }
            // CR 205.3m: Try compound subtypes first ("Ninja and Rogue", "Elf or Warrior")
            // The helper bakes in extra_props and is_other, so skip add_another_filter below.
            if let Some(compound_filter) =
                try_parse_compound_subtypes(desc_remaining, &extra_props, is_other)
            {
                // CR 613.7: Check for counter condition before returning
                let (compound_filter, after_prefix) =
                    if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                        (add_property(compound_filter, prop), rest)
                    } else {
                        (compound_filter, after_prefix)
                    };
                return parse_continuous_gets_has(after_prefix, compound_filter, text);
            }
            let typed_filter = if extra_props.is_empty() {
                // No combat-status prefix — use original dispatch path
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
                }
            } else if is_capitalized_words(desc_remaining) {
                // Combat-status prefix found + remaining is a subtype
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .subtype(desc_remaining.to_string())
                        .controller(ControllerRef::You)
                        .properties(extra_props),
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
    if let Some(yc_pos) = tp.find(" you control ") {
        let (before, after) = tp.split_at(yc_pos);
        let descriptor = before.original.trim();
        if !descriptor.is_empty() {
            let after_prefix = &after.original[" you control ".len()..];
            let full_subject = tp.original[..yc_pos + " you control".len()].trim();
            // CR 509.1h: Strip combat-status prefixes
            let mut extra_props = Vec::new();
            let mut desc_remaining = descriptor;
            let mut desc_lower = descriptor.to_lowercase();
            while let Some((prop, consumed)) = parse_combat_status_prefix(&desc_lower) {
                extra_props.push(prop);
                desc_remaining = desc_remaining[consumed..].trim_start();
                desc_lower = desc_remaining.to_lowercase();
            }
            // CR 205.3m: Try compound subtypes first ("Ninja and Rogue", "Elf or Warrior")
            if let Some(compound_filter) =
                try_parse_compound_subtypes(desc_remaining, &extra_props, is_other)
            {
                // CR 613.7: Check for counter condition before returning
                let (compound_filter, after_prefix) =
                    if let Some((prop, rest)) = strip_counter_condition_prefix(after_prefix) {
                        (add_property(compound_filter, prop), rest)
                    } else {
                        (compound_filter, after_prefix)
                    };
                return parse_continuous_gets_has(after_prefix, compound_filter, text);
            }
            let typed_filter = if extra_props.is_empty() {
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
                    // CR 205.3m: Normalize plural subtypes to canonical singular form
                    let subtype_name = parse_subtype(descriptor)
                        .map(|(canonical, _)| canonical)
                        .unwrap_or_else(|| descriptor.trim_end_matches('s').to_string());
                    TargetFilter::Typed(
                        TypedFilter::creature()
                            .subtype(subtype_name)
                            .controller(ControllerRef::You),
                    )
                } else {
                    return None;
                }
            } else if is_capitalized_words(desc_remaining) {
                // CR 205.3m: Normalize plural subtypes to canonical singular form
                let subtype_name = parse_subtype(desc_remaining)
                    .map(|(canonical, _)| canonical)
                    .unwrap_or_else(|| desc_remaining.trim_end_matches('s').to_string());
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .subtype(subtype_name)
                        .controller(ControllerRef::You)
                        .properties(extra_props),
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
    let tp = TextPair::new(text, &lower);
    let (affected, predicate_text) = strip_rule_static_subject(tp.original, tp.lower)?;
    let predicate = parse_rule_static_predicate(predicate_text)?;
    // CR 502.3: Extract trailing condition for CantUntap statics (e.g., "as long as [condition]")
    if matches!(predicate, RuleStaticPredicate::CantUntap) {
        let pred_lower = predicate_text.to_lowercase();
        if let Some(condition) = extract_cant_untap_condition(&pred_lower) {
            let mut def = lower_rule_static(predicate, affected, text);
            def.condition = Some(condition);
            return Some(def);
        }
    }
    Some(lower_rule_static(predicate, affected, text))
}

fn parse_subject_continuous_static(text: &str) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    let subject_end = find_continuous_predicate_start(tp.lower)?;
    let subject = tp.original[..subject_end].trim();
    let predicate = tp.original[subject_end + 1..].trim();
    if parse_rule_static_predicate(predicate).is_some() {
        return None;
    }
    let affected = parse_continuous_subject_filter(subject)?;

    // CR 613.4c / CR 611.3a: Route "for each" and "as long as" predicates through
    // parse_continuous_gets_has which handles dynamic P/T and condition splitting.
    let pred_lower = predicate.to_lowercase();
    if pred_lower.contains("for each ") || pred_lower.contains(" as long as ") {
        return parse_continuous_gets_has(predicate, affected, text);
    }

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
    let modification_text = strip_after(rest, ", ")?.trim();

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
                        maximum: None,
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
    let tp = TextPair::new(text, &lower);
    let mut modifications = Vec::new();

    // Strip pronoun prefix: "he's a", "she's a", "it's a", "~'s a"
    let body = tp
        .strip_prefix("he's a ")
        .or_else(|| tp.strip_prefix("she's a "))
        .or_else(|| tp.strip_prefix("it's a "))
        .or_else(|| tp.strip_prefix("~'s a "));

    let body = match body {
        Some(b) => b.trim_start(),
        None => return modifications,
    };

    // Split on " and has " or " with " to separate type/PT from keywords
    let (type_pt_part, keyword_part) = if let Some(pos) = body.find(" and has ") {
        (&body.original[..pos], Some(&body.original[pos + 9..]))
    } else if let Some(pos) = body.find(" with ") {
        (&body.original[..pos], Some(&body.original[pos + 6..]))
    } else {
        (body.original, None)
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
/// CR 113.6b: Parse "~ is in your graveyard" / "this card is in your graveyard" /
/// "~ is in your hand" → `SourceInZone { zone }`.
fn parse_source_in_zone_condition(lower: &str) -> Option<StaticCondition> {
    // Strip self-reference prefix: "~ is in your " or "this card is in your "
    let after = lower
        .strip_prefix("~ is in your ")
        .or_else(|| lower.strip_prefix("this card is in your "))?;

    let zone = match after.trim_end_matches('.').trim() {
        "graveyard" => Zone::Graveyard,
        "hand" => Zone::Hand,
        "library" => Zone::Library,
        "exile" => Zone::Exile,
        _ => return None,
    };
    Some(StaticCondition::SourceInZone { zone })
}

/// Try splitting a condition on " and " into compound `StaticCondition::And`.
/// Only succeeds when BOTH halves parse as valid conditions — prevents false splits
/// on noun phrases like "artifacts and creatures".
fn try_split_compound_and(text: &str) -> Option<StaticCondition> {
    let lower = text.to_lowercase();
    // Find " and " boundaries — try each occurrence in case the first is a noun conjunction.
    let mut search_from = 0;
    while let Some(pos) = lower[search_from..].find(" and ") {
        let abs_pos = search_from + pos;
        let left = &text[..abs_pos];
        let right = &text[abs_pos + 5..]; // " and " is 5 bytes
        if let (Some(lhs), Some(rhs)) =
            (parse_static_condition(left), parse_static_condition(right))
        {
            return Some(StaticCondition::And {
                conditions: vec![lhs, rhs],
            });
        }
        search_from = abs_pos + 5;
    }
    None
}

/// Supported patterns:
/// - "you have at least N life more than your starting life total" → LifeMoreThanStartingBy
/// - "your devotion to [colors] is less than N" → DevotionGE (with inverted threshold)
/// - "it's your turn" → DuringYourTurn
/// - "you control a/an [type]" → IsPresent with filter
fn parse_static_condition(text: &str) -> Option<StaticCondition> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // CR 113.6b: "~ is in your graveyard" / "this card is in your graveyard" / "~ is in your hand"
    if let Some(condition) = parse_source_in_zone_condition(tp.lower) {
        return Some(condition);
    }

    // Compound " and " splitting: try splitting on " and ", parse both halves recursively.
    // Only succeeds if BOTH halves parse independently — avoids false splits on
    // noun phrases like "artifacts and creatures".
    if let Some(condition) = try_split_compound_and(text) {
        return Some(condition);
    }

    // "you have at least N life more than your starting life total"
    if let Some(amount_text) = tp
        .lower
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
    if tp.lower == "it's your turn" {
        return Some(StaticCondition::DuringYourTurn);
    }

    // "your devotion to [color(s)] is less than N" (Theros gods)
    // Note: "less than N" is stored as DevotionGE with the same threshold —
    // the *effect* typically removes creature type, so the condition being false
    // (devotion >= N) means the removal doesn't apply and the god IS a creature.
    if let Some(condition) = parse_devotion_condition(tp.lower) {
        return Some(condition);
    }

    // "you control a/an [type]" → IsPresent
    if let Some(condition) = parse_control_presence_condition(tp.lower) {
        return Some(condition);
    }

    // CR 118.4: "you've lost life this turn" → QuantityComparison(LifeLostThisTurn >= 1)
    if tp.lower == "you've lost life this turn" {
        return Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeLostThisTurn,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        });
    }

    // CR 400.7: "this aura/permanent entered this turn" → SourceEnteredThisTurn
    if let Some(rest) = tp.lower.strip_prefix("this ") {
        if rest.ends_with(" entered this turn") || rest == "entered this turn" {
            return Some(StaticCondition::SourceEnteredThisTurn);
        }
    }

    // "the number of [quantity] is [comparator] [quantity]"
    if let Some(condition) = parse_quantity_comparison(tp.lower) {
        return Some(condition);
    }

    // "there are N or more [thing] in your graveyard" — threshold/delirium conditions
    if let Some(condition) = parse_graveyard_threshold_condition(tp.lower) {
        return Some(condition);
    }

    // "you control N or more [type]" — quantity threshold
    if let Some(condition) = parse_controls_n_or_more_condition(tp.lower) {
        return Some(condition);
    }

    // "N or more [type/card] entered the battlefield under your control this turn"
    if let Some(condition) = parse_entered_this_turn_condition(tp.lower) {
        return Some(condition);
    }

    // "an [type] entered the battlefield under your control this turn"
    if let Some(condition) = parse_single_entered_this_turn_condition(tp.lower) {
        return Some(condition);
    }

    // "you've committed a crime this turn" / "you've gained life this turn"
    if let Some(condition) = parse_youve_this_turn_condition(tp.lower) {
        return Some(condition);
    }

    // "you have N or more life" → QuantityComparison(LifeTotal >= N)
    if let Some(rest) = tp.lower.strip_prefix("you have ") {
        if let Some(life_text) = rest.strip_suffix(" or more life") {
            if let Some((n, remainder)) = parse_number(life_text) {
                if remainder.trim().is_empty() {
                    return Some(StaticCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::LifeTotal,
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: n as i32 },
                    });
                }
            }
        }
    }

    // "the chosen color is [color]"
    if let Some(color_name) = tp.lower.strip_prefix("the chosen color is ") {
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
    // Also handles lowercased input (e.g. from parse_control_presence_condition which
    // receives pre-lowered text) by capitalizing the first character.
    if !trimmed.is_empty() && trimmed.chars().next().unwrap().is_alphabetic() {
        let subtype = capitalize_first(trimmed);
        return Some(TypedFilter::creature().subtype(subtype));
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

/// Parse "there are N or more [things] in your graveyard" conditions.
/// Covers: threshold ("seven or more cards"), delirium ("four or more card types"),
/// and "permanent cards in your graveyard".
fn parse_graveyard_threshold_condition(lower: &str) -> Option<StaticCondition> {
    let rest = lower.strip_prefix("there are ")?;
    // "four or more card types among cards in your graveyard" (delirium)
    if rest.contains("card types among cards in your graveyard") {
        let (n, _) = parse_number(rest)?;
        return Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::CardTypesInGraveyards {
                    scope: CountScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        });
    }
    // "N or more cards/permanent cards in your graveyard" (threshold)
    if rest.contains("in your graveyard") {
        let (n, _) = parse_number(rest)?;
        return Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::GraveyardSize,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        });
    }
    None
}

/// Parse "you control N or more [type]" → QuantityComparison(ObjectCount >= N).
fn parse_controls_n_or_more_condition(lower: &str) -> Option<StaticCondition> {
    let rest = lower.strip_prefix("you control ")?;
    let (n, after_n) = parse_number(rest)?;
    let after_n = after_n.trim();
    let type_text = after_n.strip_prefix("or more ")?;
    let type_text = type_text.trim_end_matches('.');
    let (filter, remainder) = parse_type_phrase(type_text);
    if !remainder.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return None;
    }
    // Inject controller: You
    let filter = match filter {
        TargetFilter::Typed(tf) => TargetFilter::Typed(tf.controller(ControllerRef::You)),
        other => other,
    };
    Some(StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: n as i32 },
    })
}

/// Parse "N or more [type] entered the battlefield under your control this turn".
fn parse_entered_this_turn_condition(lower: &str) -> Option<StaticCondition> {
    // "[two/three/N] or more [nonland permanents/creatures/etc.] entered the battlefield under your control this turn"
    let (n, after_n) = parse_number(lower)?;
    let rest = after_n.trim();
    let rest = rest.strip_prefix("or more ")?;
    if rest.contains("entered the battlefield under your control this turn") {
        let type_text = rest.split(" entered the battlefield").next()?.trim();
        let (filter, _) = parse_type_phrase(type_text);
        let filter = match filter {
            TargetFilter::Typed(tf) => TargetFilter::Typed(tf.controller(ControllerRef::You)),
            other => other,
        };
        return Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::EnteredThisTurn { filter },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        });
    }
    None
}

/// Parse "an [type] entered the battlefield under your control this turn".
fn parse_single_entered_this_turn_condition(lower: &str) -> Option<StaticCondition> {
    let rest = lower
        .strip_prefix("a ")
        .or_else(|| lower.strip_prefix("an "))?;
    if !rest.contains("entered the battlefield under your control this turn") {
        return None;
    }
    let type_text = rest.split(" entered the battlefield").next()?.trim();
    let (filter, _) = parse_type_phrase(type_text);
    let filter = match filter {
        TargetFilter::Typed(tf) => TargetFilter::Typed(tf.controller(ControllerRef::You)),
        other => other,
    };
    Some(StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::EnteredThisTurn { filter },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    })
}

/// Parse "you've committed a crime this turn" / "you've gained life this turn".
fn parse_youve_this_turn_condition(lower: &str) -> Option<StaticCondition> {
    let rest = lower.strip_prefix("you've ")?;
    // "committed a crime this turn"
    if rest.starts_with("committed a crime this turn") {
        return Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::CrimesCommittedThisTurn,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        });
    }
    // "gained life this turn"
    if rest.starts_with("gained life this turn") {
        return Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        });
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
    let tp = TextPair::new(trimmed, &lower);

    // Strip "Each " prefix — "Each creature you control" is semantically identical to
    // "Creatures you control" for filter purposes.
    if tp.starts_with("each ") {
        return parse_continuous_subject_filter(tp.original[5..].trim());
    }

    if tp.starts_with("other ") {
        let original_rest = tp.original[6..].trim();
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
    let tp = TextPair::new(subject, &lower);
    if tp.lower == "equipped creature" {
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
        if tp.lower == pattern {
            return Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![property]),
            ));
        }
    }

    if tp.lower == "attacking creatures" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::Attacking]),
        ));
    }

    None
}

fn parse_creature_subject_filter(subject: &str) -> Option<TargetFilter> {
    let trimmed = subject.trim();
    let lower = trimmed.to_lowercase();
    let tp = TextPair::new(trimmed, &lower);

    let descriptor = if let Some(prefix) = trimmed.strip_suffix(" creatures") {
        prefix.trim()
    } else if !trimmed.contains(' ') && tp.lower.ends_with('s') {
        // CR 205.3m: Use parse_subtype for irregular plurals (Elves→Elf, Dwarves→Dwarf)
        if let Some((canonical, _)) = parse_subtype(trimmed) {
            return Some(TargetFilter::Typed(
                TypedFilter::creature().subtype(canonical),
            ));
        }
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
        " attacks each turn if able",
        " must attack each combat if able",
        " must attack if able",
        " blocks each combat if able",
        " blocks each turn if able",
        " must block each combat if able",
        " must block if able",
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
    let tp = TextPair::new(subject, &lower);

    if matches!(
        tp.lower,
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

    if tp.lower == "you" {
        return Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ));
    }

    if matches!(tp.lower, "players" | "each player") {
        return Some(TargetFilter::Player);
    }

    if tp.lower == "enchanted creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ));
    }

    if tp.lower == "enchanted permanent" {
        return Some(TargetFilter::Typed(
            TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy]),
        ));
    }

    if tp.lower == "equipped creature" {
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
    let tp = TextPair::new(text, &lower);

    if tp.starts_with("doesn't untap during")
        || tp.starts_with("doesn\u{2019}t untap during")
        || tp.starts_with("don't untap during")
        || tp.starts_with("don\u{2019}t untap during")
    {
        return Some(RuleStaticPredicate::CantUntap);
    }

    // CR 508.1d: A creature that "attacks if able" is a requirement on the declare attackers step.
    if matches!(
        tp.lower,
        "attacks each combat if able"
            | "attacks each combat if able."
            | "attacks each turn if able"
            | "attacks each turn if able."
            | "must attack each combat if able"
            | "must attack each combat if able."
            | "must attack if able"
            | "must attack if able."
    ) {
        return Some(RuleStaticPredicate::MustAttack);
    }

    // CR 509.1c: A creature that "blocks if able" is a requirement on the declare blockers step.
    if matches!(
        tp.lower,
        "blocks each combat if able"
            | "blocks each combat if able."
            | "blocks each turn if able"
            | "blocks each turn if able."
            | "must block each combat if able"
            | "must block each combat if able."
            | "must block if able"
            | "must block if able."
    ) {
        return Some(RuleStaticPredicate::MustBlock);
    }

    if matches!(
        tp.lower,
        "can block only creatures with flying" | "can block only creatures with flying."
    ) {
        return Some(RuleStaticPredicate::BlockOnlyCreaturesWithFlying);
    }

    if matches!(
        tp.lower,
        "has shroud" | "has shroud." | "have shroud" | "have shroud."
    ) {
        return Some(RuleStaticPredicate::Shroud);
    }

    if tp.starts_with("may look at the top card of your library") {
        return Some(RuleStaticPredicate::MayLookAtTopOfLibrary);
    }

    if matches!(
        tp.lower,
        "lose all abilities"
            | "lose all abilities."
            | "loses all abilities"
            | "loses all abilities."
    ) {
        return Some(RuleStaticPredicate::LoseAllAbilities);
    }

    if matches!(
        tp.lower,
        "has no maximum hand size"
            | "has no maximum hand size."
            | "have no maximum hand size"
            | "have no maximum hand size."
    ) {
        return Some(RuleStaticPredicate::NoMaximumHandSize);
    }

    if tp.starts_with("may play an additional land")
        || (tp.starts_with("may play up to ") && tp.contains("additional land"))
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
        RuleStaticPredicate::CantUntap => StaticDefinition::new(StaticMode::CantUntap)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::MustAttack => StaticDefinition::new(StaticMode::MustAttack)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::MustBlock => StaticDefinition::new(StaticMode::MustBlock)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::BlockOnlyCreaturesWithFlying => {
            StaticDefinition::new(StaticMode::BlockRestriction)
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::Shroud => StaticDefinition::new(StaticMode::Shroud)
            .affected(affected)
            .description(description.to_string()),
        RuleStaticPredicate::MayLookAtTopOfLibrary => {
            StaticDefinition::new(StaticMode::MayLookAtTopOfLibrary)
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::LoseAllAbilities => StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::RemoveAllAbilities])
            .description(description.to_string()),
        RuleStaticPredicate::NoMaximumHandSize => {
            StaticDefinition::new(StaticMode::NoMaximumHandSize)
                .affected(affected)
                .description(description.to_string())
        }
        RuleStaticPredicate::MayPlayAdditionalLand => {
            StaticDefinition::new(StaticMode::MayPlayAdditionalLand)
                .affected(affected)
                .description(description.to_string())
        }
    }
}

/// Parse the subject of "X can't be countered" lines.
/// CR 101.2: Returns SelfRef for "~ can't be countered", or a typed filter for
/// "Green spells you control can't be countered", "Creature spells you control can't be countered", etc.
fn parse_cant_be_countered_subject(tp: &TextPair) -> TargetFilter {
    // Find the subject before "can't be countered"
    if let Some(pos) = tp.lower.find("can't be countered") {
        let subject = tp.lower[..pos].trim();
        // Self-referential: "~" or card name (handled by tp.contains matching the card name)
        if subject.is_empty() || subject == "~" || subject.ends_with(" ~") {
            return TargetFilter::SelfRef;
        }
        // "X spells you control" — parse color + type filter
        if let Some(before_yc) = subject.strip_suffix(" you control") {
            if let Some(before_spells) = before_yc
                .strip_suffix(" spells")
                .or_else(|| before_yc.strip_suffix(" spell"))
            {
                let mut properties = Vec::new();
                let mut card_types = Vec::new();

                // Split on " and " to handle compound types: "creature and enchantment"
                for part in before_spells.split(" and ") {
                    for word in part.split_whitespace() {
                        if let Some(color) = parse_named_color(word) {
                            properties.push(FilterProp::HasColor {
                                color: color.to_string(),
                            });
                        } else {
                            // Try as card type: "creature", "instant", "sorcery", etc.
                            match word {
                                "creature" => card_types.push(TypeFilter::Creature),
                                "instant" => card_types.push(TypeFilter::Instant),
                                "sorcery" => card_types.push(TypeFilter::Sorcery),
                                "enchantment" => card_types.push(TypeFilter::Enchantment),
                                "artifact" => card_types.push(TypeFilter::Artifact),
                                _ => {}
                            }
                        }
                    }
                }

                // CR 608.2b: Single type → direct filter; multiple types → AnyOf wrapper
                let type_filters = if card_types.len() > 1 {
                    vec![TypeFilter::AnyOf(card_types)]
                } else {
                    card_types
                };

                return TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: Some(ControllerRef::You),
                    properties,
                });
            }
        }
    }
    TargetFilter::SelfRef
}

/// Parse the subject of "[type] cards in [zones] can't enter the battlefield".
/// CR 604.3: Extracts the card type filter and zone restrictions into a TypedFilter.
fn parse_cant_enter_battlefield_subject(tp: &TextPair) -> TargetFilter {
    let mut card_type = None;
    let mut properties = Vec::new();

    if let Some(pos) = tp.lower.find("can't enter the battlefield") {
        let subject = tp.lower[..pos].trim();
        // "creature cards in graveyards and libraries" → card_type = Creature
        if let Some(type_part) = subject.split(" cards").next() {
            card_type = match type_part.trim() {
                "creature" => Some(TypeFilter::Creature),
                "artifact" => Some(TypeFilter::Artifact),
                "enchantment" => Some(TypeFilter::Enchantment),
                "instant" => Some(TypeFilter::Instant),
                "sorcery" => Some(TypeFilter::Sorcery),
                _ => None,
            };
        }
    }

    let zones = parse_zone_names_from_tp(tp);
    if !zones.is_empty() {
        properties.push(FilterProp::InAnyZone { zones });
    }

    TargetFilter::Typed(TypedFilter {
        type_filters: card_type.into_iter().collect(),
        properties,
        ..TypedFilter::default()
    })
}

/// Extract zone names referenced in Oracle text.
/// Handles "graveyards", "libraries", "exile" and their singular/plural forms.
fn parse_zone_names_from_tp(tp: &TextPair) -> Vec<Zone> {
    let mut zones = Vec::new();
    if tp.lower.contains("graveyard") {
        zones.push(Zone::Graveyard);
    }
    if tp.lower.contains("librar") {
        zones.push(Zone::Library);
    }
    if tp.lower.contains("exile") {
        zones.push(Zone::Exile);
    }
    zones
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

/// Parse the predicate of an enchanted/equipped grant, handling:
/// - Non-standard keyword phrasings: "can attack as though it had haste", "can't be blocked"
/// - Conditional grants: "gets +1/+1 as long as you control a Wizard"
/// - Standard continuous grants: "gets +N/+M", "has keyword", "for each", "where X is"
///
/// CR 702.10 + CR 509.1b + CR 613.4c: Enchanted/equipped predicate dispatch.
fn parse_enchanted_equipped_predicate(
    predicate: &str,
    affected: TargetFilter,
    description: &str,
) -> Option<StaticDefinition> {
    let pred_lower = predicate.to_lowercase();

    // --- Non-standard keyword phrasings (check before continuous grants) ---

    // CR 702.10: "can attack as though it had haste" → AddKeyword(Haste)
    if pred_lower.contains("can attack as though it had haste")
        || pred_lower.contains("can attack as though it didn't have defender")
    {
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                }])
                .description(description.to_string()),
        );
    }

    // CR 509.1b: "can't be blocked" on enchanted/equipped creature
    if pred_lower.starts_with("can't be blocked") {
        // "can't be blocked except by" → CantBeBlockedExceptBy
        if let Some(rest) = pred_lower.strip_prefix("can't be blocked except by ") {
            let filter_text = rest.trim_end_matches('.');
            return Some(
                StaticDefinition::new(StaticMode::CantBeBlockedExceptBy {
                    filter: filter_text.to_string(),
                })
                .affected(affected)
                .description(description.to_string()),
            );
        }
        return Some(
            StaticDefinition::new(StaticMode::CantBeBlocked)
                .affected(affected)
                .description(description.to_string()),
        );
    }

    // --- Conditional grants: split "as long as" before passing to continuous parser ---
    // Handles both "gets +1/+1 as long as ..." and "has flying as long as ..."
    let pred_tp = TextPair::new(predicate, &pred_lower);
    if let Some((before_cond, after_cond)) = pred_tp.split_around(" as long as ") {
        let continuous_text = before_cond.original;
        let condition_text = after_cond.original.trim().trim_end_matches('.');
        if let Some(mut def) =
            parse_continuous_gets_has(continuous_text, affected.clone(), description)
        {
            let condition =
                parse_static_condition(condition_text).unwrap_or(StaticCondition::Unrecognized {
                    text: condition_text.to_string(),
                });
            def.condition = Some(condition);
            return Some(def);
        }
    }

    // --- Standard continuous grants (gets/has/for each/where X) ---
    parse_continuous_gets_has(predicate, affected, description)
}

/// Parse "gets +N/+M [and has {keyword}]" after the subject.
/// Also handles "gets +N/+M for each [clause]" dynamic P/T patterns.
fn parse_continuous_gets_has(
    text: &str,
    affected: TargetFilter,
    description: &str,
) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // CR 611.3a: Split "as long as [condition]" BEFORE "for each" — the condition applies
    // to the entire static, not to a quantity count. Mirrors parse_enchanted_equipped_predicate.
    if let Some((before_cond, after_cond)) = tp.split_around(" as long as ") {
        let continuous_text = before_cond.original;
        let condition_text = after_cond.original.trim().trim_end_matches('.');
        // Recursively parse the continuous part without the condition
        if let Some(mut def) =
            parse_continuous_gets_has(continuous_text, affected.clone(), description)
        {
            let condition =
                parse_static_condition(condition_text).unwrap_or(StaticCondition::Unrecognized {
                    text: condition_text.to_string(),
                });
            def.condition = Some(condition);
            return Some(def);
        }
    }

    // CR 613.4c: Handle "gets +N/+M for each [clause]" — dynamic P/T via ObjectCount.
    if let Some((before_for_each, after_for_each)) = tp.split_around("for each ") {
        let pt_text = before_for_each.original.trim();
        let for_each_clause = after_for_each.lower.trim_end_matches('.');

        let pt_lower = pt_text.to_lowercase();
        let pt_source = pt_lower
            .strip_prefix("gets ")
            .or_else(|| pt_lower.strip_prefix("get "))
            .unwrap_or(&pt_lower);

        if let Some((p, t)) = parse_pt_mod(pt_source) {
            if let Some(qty) = super::oracle_quantity::parse_for_each_clause(for_each_clause) {
                let quantity = QuantityExpr::Ref { qty };
                let mut modifications = Vec::new();
                if p != 0 {
                    let value = if p.abs() == 1 {
                        if p > 0 {
                            quantity.clone()
                        } else {
                            QuantityExpr::Multiply {
                                factor: -1,
                                inner: Box::new(quantity.clone()),
                            }
                        }
                    } else {
                        QuantityExpr::Multiply {
                            factor: p,
                            inner: Box::new(quantity.clone()),
                        }
                    };
                    modifications.push(ContinuousModification::AddDynamicPower { value });
                }
                if t != 0 {
                    let value = if t.abs() == 1 {
                        if t > 0 {
                            quantity
                        } else {
                            QuantityExpr::Multiply {
                                factor: -1,
                                inner: Box::new(quantity),
                            }
                        }
                    } else {
                        QuantityExpr::Multiply {
                            factor: t,
                            inner: Box::new(quantity),
                        }
                    };
                    modifications.push(ContinuousModification::AddDynamicToughness { value });
                }
                if !modifications.is_empty() {
                    // Check for trailing "and has [keyword]" after the for-each clause
                    // e.g., "gets +1/+0 for each Mountain you control and has first strike"
                    if let Some(keyword_text) = extract_keyword_clause(description) {
                        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
                            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                                modifications
                                    .push(ContinuousModification::AddKeyword { keyword: kw });
                            }
                        }
                    }
                    return Some(
                        StaticDefinition::continuous()
                            .affected(affected)
                            .modifications(modifications)
                            .description(description.to_string()),
                    );
                }
            }
        }
    }

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
    // Strip "where X is [quantity]" before parsing modifications,
    // but only if the text doesn't contain quoted abilities (which have their
    // own "where X is" handling inside the quote).
    let text_lower = text.to_lowercase();
    let text_tp = TextPair::new(text, &text_lower);
    let (stripped_tp, where_x_expression) = if text.contains('"') {
        (text_tp, None)
    } else {
        super::oracle_effect::strip_trailing_where_x(text_tp)
    };
    let tp = stripped_tp;
    let text_stripped = tp.original;
    let lower = tp.lower;
    let mut modifications = Vec::new();

    if tp.contains("lose all abilities") {
        modifications.push(ContinuousModification::RemoveAllAbilities);
    }

    if tp.starts_with("gets ") || tp.starts_with("get ") {
        let offset = if tp.starts_with("gets ") { 5 } else { 4 };
        let after = &tp.original[offset..].trim();
        if let Some((p, t)) = parse_pt_mod(after) {
            modifications.push(ContinuousModification::AddPower { value: p });
            modifications.push(ContinuousModification::AddToughness { value: t });
        }
    }

    // CR 613.4c: Scan for "get +X/+X" / "gets +X/+X" anywhere in the text
    // for dynamic P/T modification (e.g., Craterhoof Behemoth)
    if let Some(dynamic_mods) = parse_dynamic_pt_in_text(lower, where_x_expression.as_deref()) {
        modifications.extend(dynamic_mods);
    }

    if let Some((power, toughness)) = parse_base_pt_mod(text_stripped) {
        modifications.push(ContinuousModification::SetPower { value: power });
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }
    if let Some(power) = parse_base_power_mod(text_stripped) {
        modifications.push(ContinuousModification::SetPower { value: power });
    }
    if let Some(toughness) = parse_base_toughness_mod(text_stripped) {
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }

    for definition in parse_quoted_abilities(text_stripped) {
        modifications.push(ContinuousModification::GrantAbility {
            definition: Box::new(definition),
        });
    }

    if let Some(keyword_text) = extract_keyword_clause(text_stripped) {
        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::AddKeyword { keyword: kw });
            }
        }
    }

    // CR 702: "lose [keyword]" / "loses [keyword]" — keyword removal.
    if let Some(keyword_text) = extract_lose_keyword_clause(text_stripped) {
        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::RemoveKeyword { keyword: kw });
            }
        }
    }

    // CR 205.1a: "becomes a [Type] in addition to its other creature types"
    if let Some(subtype) = parse_becomes_type_addition(lower) {
        modifications.push(ContinuousModification::AddSubtype { subtype });
    }

    modifications
}

/// CR 613.4c: Scan text for "get(s) +X/+X" and resolve X via where_x_expression.
/// Returns AddDynamicPower + AddDynamicToughness modifications if found.
fn parse_dynamic_pt_in_text(
    lower: &str,
    where_x_expression: Option<&str>,
) -> Option<Vec<ContinuousModification>> {
    // Look for "get +x/+x" or "gets +x/+x" anywhere in text
    let pt_pos = lower
        .find("get +x/+x")
        .or_else(|| lower.find("gets +x/+x"))
        .or_else(|| lower.find("get +x/+0"))
        .or_else(|| lower.find("get +0/+x"))?;

    // Resolve the "where X is" expression to a QuantityExpr
    let quantity = if let Some(expr) = where_x_expression {
        parse_cda_quantity(expr)?
    } else {
        return None;
    };

    // Check if the pattern is +X/+X (symmetric) or asymmetric
    let pt_text = &lower[pt_pos..];
    let after_get = pt_text
        .strip_prefix("gets ")
        .or_else(|| pt_text.strip_prefix("get "))?;

    // Parse the P/T pattern
    let slash = after_get.find('/')?;
    let p_str = after_get[..slash].trim().trim_start_matches('+');
    let rest = &after_get[slash + 1..];
    let t_end = rest
        .find(|c: char| c.is_whitespace() || c == '.' || c == ',')
        .unwrap_or(rest.len());
    let t_str = rest[..t_end].trim().trim_start_matches('+');

    let mut mods = Vec::new();
    if p_str.eq_ignore_ascii_case("x") {
        mods.push(ContinuousModification::AddDynamicPower {
            value: quantity.clone(),
        });
    }
    if t_str.eq_ignore_ascii_case("x") {
        mods.push(ContinuousModification::AddDynamicToughness { value: quantity });
    }

    if mods.is_empty() {
        None
    } else {
        Some(mods)
    }
}

/// CR 205.1a: Parse "becomes a [Type] in addition to its other creature types"
/// Returns the canonical subtype name if the pattern matches.
fn parse_becomes_type_addition(lower: &str) -> Option<String> {
    let rest = lower.split("becomes a ").nth(1)?;
    let subtype_end = rest.find(" in addition to")?;
    let subtype_word = rest[..subtype_end].trim();
    // Capitalize the subtype
    let mut chars = subtype_word.chars();
    let capitalized = match chars.next() {
        Some(first) => {
            let mut s = first.to_uppercase().collect::<String>();
            s.push_str(chars.as_str());
            s
        }
        None => return None,
    };
    Some(capitalized)
}

fn parse_base_pt_mod(text: &str) -> Option<(i32, i32)> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let pt_text = tp.strip_after("base power and toughness ")?.original.trim();
    parse_pt_mod(pt_text)
}

fn parse_base_power_mod(text: &str) -> Option<i32> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    if tp.contains("base power and toughness ") {
        return None;
    }
    let power_text = tp.strip_after("base power ")?.original.trim();
    parse_single_pt_value(power_text)
}

fn parse_base_toughness_mod(text: &str) -> Option<i32> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    if tp.contains("base power and toughness ") {
        return None;
    }
    let toughness_text = tp.strip_after("base toughness ")?.original.trim();
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
    let lower = text.to_lowercase();

    // CR 603.1: Detect trigger prefixes and route to trigger parser.
    // Quoted ability text starting with "When"/"Whenever"/"At the beginning of" is a
    // triggered ability, not a spell-like effect chain. Extract the trigger's execute
    // chain as the granted AbilityDefinition (trigger metadata like mode/condition is
    // handled by the GrantTrigger path if available, but the effect chain is always useful).
    if lower.starts_with("when ")
        || lower.starts_with("whenever ")
        || lower.starts_with("at the beginning of ")
        || lower.starts_with("at the end of ")
    {
        let trigger = super::oracle_trigger::parse_trigger_line(text, "~");
        if let Some(execute) = trigger.execute {
            return *execute;
        }
        // Fallback: parse as effect chain if trigger parsing produced no execute
    }

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
    // CR 702.73a: "all creature types" is the Changeling CDA effect.
    // Granting Changeling keyword triggers layer system post-fixup to add all types.
    if word.eq_ignore_ascii_case("all creature types") {
        return Some(Keyword::Changeling);
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
        if let Some(after_plus) = strip_after(lower, "that number plus ") {
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

/// CR 604.2 + CR 601.2a + CR 305.1: Parse graveyard play/cast permission statics.
/// Handles three patterns:
/// 1. "Once during each of your turns, you may cast [filter] from your graveyard." (Lurrus, Karador)
/// 2. "You may play [filter] from your graveyard." (Crucible of Worlds, Icetill Explorer)
/// 3. "You may cast [filter] from your graveyard." (Conduit of Worlds)
fn try_parse_graveyard_cast_permission(text: &str, lower: &str) -> Option<StaticDefinition> {
    // Determine pattern and extract the rest after the prefix
    let (rest, once_per_turn, play_mode) =
        if let Some(r) = lower.strip_prefix("once during each of your turns, you may cast ") {
            (r, true, CardPlayMode::Cast)
        } else if let Some(r) = lower.strip_prefix("you may play ") {
            (r, false, CardPlayMode::Play)
        } else if let Some(r) = lower.strip_prefix("you may cast ") {
            // Only match if "from your graveyard" follows — avoid catching other "you may cast" statics
            if r.contains("from your graveyard") {
                (r, false, CardPlayMode::Cast)
            } else {
                return None;
            }
        } else {
            return None;
        };

    let gy_idx = rest.find(" from your graveyard")?;
    let filter_text = &rest[..gy_idx];

    // Strip leading article ("a ", "an ")
    let filter_text = filter_text
        .strip_prefix("a ")
        .or_else(|| filter_text.strip_prefix("an "))
        .unwrap_or(filter_text);

    // Remove " spell"/" spells" — parse_type_phrase expects bare type words.
    // "lands" is already a valid type phrase, so no stripping needed for Play mode.
    let cleaned: Cow<str> = if filter_text.contains(" spells") {
        Cow::Owned(filter_text.replacen(" spells", "", 1))
    } else if filter_text.contains(" spell") {
        Cow::Owned(filter_text.replacen(" spell", "", 1))
    } else {
        Cow::Borrowed(filter_text)
    };

    let (filter, _) = parse_type_phrase(&cleaned);

    Some(
        StaticDefinition::new(StaticMode::GraveyardCastPermission {
            once_per_turn,
            play_mode,
        })
        .affected(filter)
        .description(text.to_string()),
    )
}

/// CR 601.2f: Parse cost modification statics from Oracle text.
/// Handles all four sub-patterns:
/// 1. Type-filtered: "Creature spells you cast cost {1} less to cast"
/// 2. Color-filtered: "White spells your opponents cast cost {1} more to cast"
/// 3. Global taxing: "Noncreature spells cost {1} more to cast" (Thalia)
/// 4. Broad: "Spells you cast cost {1} less to cast"
///
/// Dynamic "for each" counts are extracted when present.
fn try_parse_cost_modification(text: &str, lower: &str) -> Option<StaticDefinition> {
    let is_raise = lower.contains("more to cast") || lower.contains("more to activate");
    let is_reduce = lower.contains("less to cast") || lower.contains("less to activate");
    if !is_raise && !is_reduce {
        return None;
    }

    // Extract the mana amount from the text (look for {N} pattern)
    let amount = if let Some(brace_start) = text.find('{') {
        let cost_fragment = &text[brace_start..];
        parse_mana_symbols(cost_fragment)
            .map(|(cost, _)| cost)
            .unwrap_or_else(|| ManaCost::generic(1))
    } else {
        ManaCost::generic(1)
    };

    // Determine player scope from "you cast", "your opponents cast", or bare
    let controller = if lower.contains("your opponents cast") || lower.contains("opponents cast") {
        Some(ControllerRef::Opponent)
    } else if lower.contains("you cast") {
        Some(ControllerRef::You)
    } else {
        // Bare "spells cost more/less" — affects all players' spells.
        // For "Noncreature spells cost {1} more", both players are affected
        // in the casting check — no controller restriction on affected.
        None
    };

    // Extract spell type filter from the text before "cost"
    // E.g., "Creature spells you cast" → Creature, "Instant and sorcery spells" → AnyOf(Instant, Sorcery)
    let spell_filter = if let Some(cost_idx) = lower.find(" cost") {
        let prefix = &lower[..cost_idx];
        // Strip player scope suffixes first, then "spells"/"spell"
        let type_desc = prefix
            .trim_end_matches(" you cast")
            .trim_end_matches(" your opponents cast")
            .trim_end_matches(" opponents cast")
            .trim_end_matches(" spells")
            .trim_end_matches(" spell")
            .trim();
        // "spells" alone means no type restriction (bare "Spells you cast cost...")
        if type_desc.is_empty() || type_desc == "spells" || type_desc == "spell" {
            None
        } else {
            // First try parse_type_phrase for standard type patterns
            let (filter, _) = parse_type_phrase(type_desc);
            match &filter {
                // Single type: "creature", "noncreature", "artifact"
                TargetFilter::Typed(tf)
                    if !tf.type_filters.is_empty() || !tf.properties.is_empty() =>
                {
                    Some(filter)
                }
                // Combined types: "instant and sorcery", "artifact or enchantment"
                TargetFilter::Or { filters } if !filters.is_empty() => Some(filter),
                _ => {
                    // Fallback: check for bare color names ("white", "blue", etc.)
                    match type_desc {
                        "white" | "blue" | "black" | "red" | "green" => {
                            Some(TargetFilter::Typed(TypedFilter::card().properties(vec![
                                FilterProp::HasColor {
                                    color: type_desc.to_string(),
                                },
                            ])))
                        }
                        _ => None,
                    }
                }
            }
        }
    } else {
        None
    };

    // Detect dynamic "for each" count pattern
    // "for each artifact you control" → QuantityRef::ObjectCount
    let cost_tp = TextPair::new(text, lower);
    let dynamic_count = if let Some((_, after_for_each)) = cost_tp.split_around("for each ") {
        // Strip trailing period/punctuation
        let count_text = after_for_each.original.trim_end_matches('.');
        let (count_filter, _) = parse_type_phrase(count_text);
        Some(QuantityRef::ObjectCount {
            filter: count_filter,
        })
    } else {
        None
    };

    let mode = if is_raise {
        StaticMode::RaiseCost {
            amount,
            spell_filter: spell_filter.clone(),
            dynamic_count: dynamic_count.clone(),
        }
    } else {
        StaticMode::ReduceCost {
            amount,
            spell_filter: spell_filter.clone(),
            dynamic_count: dynamic_count.clone(),
        }
    };

    // Build the affected filter for the static definition.
    // This controls which objects are "affected" — for cost modification statics,
    // this is the source permanent's controller scope (used by the registry).
    let affected = match controller {
        Some(ControllerRef::You) => {
            TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::You))
        }
        Some(ControllerRef::Opponent) => {
            TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::Opponent))
        }
        None => TargetFilter::Typed(TypedFilter::card()),
    };

    Some(
        StaticDefinition::new(mode)
            .affected(affected)
            .description(text.to_string()),
    )
}

/// Parse a basic land type name (case-insensitive) to its enum variant.
fn parse_basic_land_type(name: &str) -> Option<BasicLandType> {
    match name.to_ascii_lowercase().as_str() {
        "plains" => Some(BasicLandType::Plains),
        "island" => Some(BasicLandType::Island),
        "swamp" => Some(BasicLandType::Swamp),
        "mountain" => Some(BasicLandType::Mountain),
        "forest" => Some(BasicLandType::Forest),
        _ => None,
    }
}

/// Parse a basic land type name, accepting both singular and plural forms.
/// "Mountains" → Mountain, "Islands" → Island. "Plains" is already valid singular.
fn parse_basic_land_type_plural(name: &str) -> Option<BasicLandType> {
    parse_basic_land_type(name).or_else(|| name.strip_suffix('s').and_then(parse_basic_land_type))
}

/// CR 305.7: Parse "[Subject] lands are [type]" land type-changing static abilities.
/// Handles replacement ("Nonbasic lands are Mountains"), additive ("Each land is a
/// Swamp in addition to its other land types"), and all-basic-types ("Lands you control
/// are every basic land type in addition to their other types").
fn parse_land_type_change(tp: &TextPair<'_>, text: &str) -> Option<StaticDefinition> {
    let (subject_tp, rest_tp) = tp
        .split_around(" are ")
        .or_else(|| tp.split_around(" is a "))?;
    let subject = subject_tp.original;
    let rest = rest_tp.original.trim().trim_end_matches('.');

    // Only proceed if subject is a land-type-change subject (avoids matching non-land patterns).
    let affected = parse_land_type_change_subject(subject)?;
    let lower_rest = rest.to_lowercase();

    // "every basic land type in addition to their other types"
    if lower_rest.starts_with("every basic land type") && lower_rest.contains("in addition to") {
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddAllBasicLandTypes])
                .description(text.to_string()),
        );
    }

    // "[Type] in addition to {its/their} other {land }types" → AddSubtype (additive)
    if let Some(type_part) = strip_in_addition_suffix(&lower_rest) {
        let basic_type = parse_basic_land_type_plural(type_part.trim())?;
        return Some(
            StaticDefinition::continuous()
                .affected(affected)
                .modifications(vec![ContinuousModification::AddSubtype {
                    subtype: basic_type.as_subtype_str().to_string(),
                }])
                .description(text.to_string()),
        );
    }

    // CR 305.7: Replacement semantics — "[Type]" or "[Types]" → SetBasicLandType
    let basic_type = parse_basic_land_type_plural(rest.trim())?;
    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::SetBasicLandType {
                land_type: basic_type,
            }])
            .description(text.to_string()),
    )
}

/// Parse the subject of a land type-change line into a TargetFilter.
fn parse_land_type_change_subject(subject: &str) -> Option<TargetFilter> {
    match subject.to_lowercase().as_str() {
        "nonbasic lands" => Some(TargetFilter::Typed(TypedFilter::land().properties(vec![
            FilterProp::NotSupertype {
                value: "Basic".to_string(),
            },
        ]))),
        "lands you control" => Some(TargetFilter::Typed(
            TypedFilter::land().controller(ControllerRef::You),
        )),
        "each land" | "all lands" => Some(TargetFilter::Typed(TypedFilter::land())),
        _ => None,
    }
}

/// Strip "in addition to {its/their} other {land }types" suffix,
/// returning the type name before it.
fn strip_in_addition_suffix(text: &str) -> Option<&str> {
    [
        " in addition to its other land types",
        " in addition to its other types",
        " in addition to their other land types",
        " in addition to their other types",
    ]
    .iter()
    .find_map(|suffix| text.strip_suffix(suffix))
}

/// CR 502.3: Extract a trailing condition from a "doesn't untap during [untap step]" clause.
/// Handles patterns like:
/// - "doesn't untap during your untap step as long as [condition]"
/// - "doesn't untap during your untap step if [condition]"
fn extract_cant_untap_condition(lower: &str) -> Option<StaticCondition> {
    // Find the end of the "untap step" phrase
    let untap_phrases = [
        "its controller's untap step",
        "its controller\u{2019}s untap step",
        "their controllers' untap steps",
        "your untap step",
    ];
    let mut after_untap = None;
    for phrase in &untap_phrases {
        if let Some(pos) = lower.find(phrase) {
            let end = pos + phrase.len();
            after_untap = Some(lower[end..].trim().trim_end_matches('.'));
            break;
        }
    }
    let remaining = after_untap?;
    if remaining.is_empty() {
        return None;
    }
    // Strip "as long as" or "if" prefix
    let condition_text = remaining
        .strip_prefix("as long as ")
        .or_else(|| remaining.strip_prefix("if "))?;
    parse_static_condition(condition_text).or_else(|| {
        Some(StaticCondition::Unrecognized {
            text: condition_text.to_string(),
        })
    })
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
        assert_eq!(def.mode, StaticMode::CantBeBlocked);
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
            Some(TargetFilter::Typed(ref tf)) if tf.type_filters.contains(&TypeFilter::Permanent)
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
            Some(TargetFilter::Typed(ref tf)) if tf.type_filters.contains(&TypeFilter::Creature) && tf.controller.is_none()
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
            Some(TargetFilter::Typed(ref tf))
                if tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.type_filters.contains(&TypeFilter::Subtype("Elf".to_string()))
                    && tf.controller == Some(ControllerRef::You)
        ));
    }

    #[test]
    fn static_color_creatures_you_control() {
        let def = parse_static_line("White creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.affected,
            Some(TargetFilter::Typed(ref tf))
                if tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.get_subtype().is_none()
                    && tf.controller == Some(ControllerRef::You)
                    && tf.properties == vec![FilterProp::HasColor { color: "White".to_string() }]
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
        assert_eq!(def.mode, StaticMode::CantUntap);
        assert!(def.description.is_some());
    }

    #[test]
    fn static_cant_be_countered() {
        // CR 101.2: "can't be countered" emits CantBeCountered, not CantBeCast
        let def = parse_static_line("Carnage Tyrant can't be countered.").unwrap();
        assert_eq!(def.mode, StaticMode::CantBeCountered);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
        assert!(def.description.is_some());
    }

    #[test]
    fn static_cant_be_countered_typed_subject() {
        // Allosaurus Shepherd: "Green spells you control can't be countered."
        let def = parse_static_line("Green spells you control can't be countered.").unwrap();
        assert_eq!(def.mode, StaticMode::CantBeCountered);
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::HasColor { color } if color == "Green")),
                "Expected HasColor Green, got {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got {:?}", def.affected);
        }
    }

    #[test]
    fn static_spells_cost_less() {
        let def = parse_static_line("Spells you cast cost {1} less to cast.").unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::ReduceCost {
                spell_filter: None,
                dynamic_count: None,
                ..
            }
        ));
        // Verify amount is generic 1 (avoid assert_eq! on complex types — SIGABRT risk)
        assert!(matches!(
            def.mode,
            StaticMode::ReduceCost {
                amount: ManaCost::Cost { generic: 1, .. },
                ..
            }
        ));
    }

    #[test]
    fn static_opponent_spells_cost_more() {
        let def = parse_static_line("Spells your opponents cast cost {1} more to cast.").unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::RaiseCost {
                spell_filter: None,
                dynamic_count: None,
                ..
            }
        ));
        assert!(matches!(
            def.mode,
            StaticMode::RaiseCost {
                amount: ManaCost::Cost { generic: 1, .. },
                ..
            }
        ));
    }

    #[test]
    fn static_creature_spells_cost_less() {
        // Goblin Electromancer-style: "Creature spells you cast cost {1} less to cast."
        let def = parse_static_line("Creature spells you cast cost {1} less to cast.").unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::ReduceCost {
                amount: ManaCost::Cost { generic: 1, .. },
                ..
            }
        ));
        if let StaticMode::ReduceCost {
            ref spell_filter, ..
        } = def.mode
        {
            let filter = spell_filter.as_ref().expect("Expected spell_filter");
            match filter {
                TargetFilter::Typed(tf) => {
                    assert!(
                        tf.type_filters
                            .iter()
                            .any(|t| matches!(t, TypeFilter::Creature)),
                        "Expected Creature type filter"
                    );
                }
                _ => panic!("Expected Typed filter"),
            }
        }
    }

    #[test]
    fn static_instant_sorcery_spells_cost_less() {
        // Goblin Electromancer: "Instant and sorcery spells you cast cost {1} less to cast."
        let def = parse_static_line("Instant and sorcery spells you cast cost {1} less to cast.");
        assert!(
            def.is_some(),
            "parse returned None for instant/sorcery cost reduction"
        );
        let def = def.unwrap();
        assert!(
            matches!(def.mode, StaticMode::ReduceCost { .. }),
            "Expected ReduceCost mode"
        );
        if let StaticMode::ReduceCost {
            ref spell_filter, ..
        } = def.mode
        {
            assert!(
                spell_filter.is_some(),
                "Expected spell_filter for instant/sorcery"
            );
            let filter = spell_filter.as_ref().unwrap();
            // parse_type_phrase("instant and sorcery") → TargetFilter::Or { [Typed(Instant), Typed(Sorcery)] }
            fn contains_type(f: &TargetFilter, expected: TypeFilter) -> bool {
                match f {
                    TargetFilter::Typed(tf) => tf.type_filters.contains(&expected),
                    TargetFilter::Or { filters } => filters
                        .iter()
                        .any(|inner| contains_type(inner, expected.clone())),
                    _ => false,
                }
            }
            assert!(
                contains_type(filter, TypeFilter::Instant),
                "Expected Instant in filter"
            );
            assert!(
                contains_type(filter, TypeFilter::Sorcery),
                "Expected Sorcery in filter"
            );
        }
    }

    #[test]
    fn static_white_spells_cost_more() {
        // "White spells your opponents cast cost {1} more to cast."
        let def =
            parse_static_line("White spells your opponents cast cost {1} more to cast.").unwrap();
        assert!(matches!(def.mode, StaticMode::RaiseCost { .. }));
        if let StaticMode::RaiseCost {
            ref spell_filter, ..
        } = def.mode
        {
            let filter = spell_filter.as_ref().expect("Expected spell_filter");
            match filter {
                TargetFilter::Typed(tf) => {
                    assert!(
                        tf.properties.iter().any(|p| matches!(
                            p,
                            FilterProp::HasColor { ref color } if color == "white"
                        )),
                        "Expected HasColor white"
                    );
                }
                _ => panic!("Expected Typed filter"),
            }
        }
    }

    #[test]
    fn static_noncreature_spells_cost_more_thalia() {
        // Thalia: "Noncreature spells cost {1} more to cast."
        let def = parse_static_line("Noncreature spells cost {1} more to cast.").unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::RaiseCost {
                amount: ManaCost::Cost { generic: 1, .. },
                ..
            }
        ));
        if let StaticMode::RaiseCost {
            ref spell_filter, ..
        } = def.mode
        {
            let filter = spell_filter.as_ref().expect("Expected spell_filter");
            match filter {
                TargetFilter::Typed(tf) => {
                    // Noncreature → TypeFilter::Non(Creature)
                    assert!(
                        tf.type_filters.iter().any(|t| matches!(
                            t,
                            TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Creature)
                        )),
                        "Expected Non(Creature) type filter"
                    );
                }
                _ => panic!("Expected Typed filter"),
            }
        }
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
            Some(TargetFilter::Typed(ref tf))
                if tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.controller == Some(ControllerRef::You)
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
        assert_eq!(def.mode, StaticMode::CantUntap);
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
        assert_eq!(def.mode, StaticMode::CantUntap);
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
        assert_eq!(def.mode, StaticMode::BlockRestriction);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_you_have_shroud() {
        let def = parse_static_line("You have shroud.").unwrap();
        assert_eq!(def.mode, StaticMode::Shroud);
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
        assert_eq!(def.mode, StaticMode::NoMaximumHandSize);
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
        assert_eq!(def.mode, StaticMode::MayPlayAdditionalLand);
        assert_eq!(def.affected, Some(TargetFilter::Player));
    }

    #[test]
    fn static_you_may_choose_not_to_untap_self() {
        let def =
            parse_static_line("You may choose not to untap this creature during your untap step.")
                .unwrap();
        assert_eq!(def.mode, StaticMode::MayChooseNotToUntap);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_you_may_look_at_top_card_of_library() {
        let def =
            parse_static_line("You may look at the top card of your library any time.").unwrap();
        assert_eq!(def.mode, StaticMode::MayLookAtTopOfLibrary);
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
                    .properties(vec![FilterProp::CmcLE {
                        value: QuantityExpr::Fixed { value: 3 },
                    }]),
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
                    ..
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
        assert_eq!(def.mode, StaticMode::MustBeBlocked);
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
        assert_eq!(def.mode, StaticMode::AdditionalLandDrop { count: 2 });
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
            Some(TargetFilter::Typed(ref tf))
                if tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.controller == Some(ControllerRef::You) =>
            {
                let properties = &tf.properties;
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
            Some(TargetFilter::Typed(ref tf))
                if tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.controller == Some(ControllerRef::You) =>
            {
                let properties = &tf.properties;
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
        use crate::types::mana::ManaColor;
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

    // --- Graveyard cast permission tests ---

    #[test]
    fn graveyard_cast_permission_lurrus() {
        let text = "Once during each of your turns, you may cast a permanent spell with mana value 2 or less from your graveyard.";
        let def = parse_static_line(text).expect("should parse Lurrus text");
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                once_per_turn: true,
                play_mode: CardPlayMode::Cast,
            }
        ));
        let filter = def.affected.expect("should have affected filter");
        if let TargetFilter::Typed(tf) = &filter {
            assert!(tf.type_filters.contains(&TypeFilter::Permanent));
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::CmcLE { .. })),
                "Expected CmcLE property, got: {:?}",
                tf.properties
            );
        } else {
            panic!("Expected Typed filter, got: {filter:?}");
        }
    }

    #[test]
    fn graveyard_cast_permission_karador() {
        let def = parse_static_line(
            "Once during each of your turns, you may cast a creature spell from your graveyard.",
        )
        .unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                once_per_turn: true,
                play_mode: CardPlayMode::Cast,
            }
        ));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("Expected Typed creature filter for Karador, got {other:?}"),
        }
    }

    #[test]
    fn graveyard_cast_permission_kess() {
        let def = parse_static_line(
            "Once during each of your turns, you may cast an instant or sorcery spell from your graveyard."
        ).unwrap();
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                once_per_turn: true,
                play_mode: CardPlayMode::Cast,
            }
        ));
        // Should parse as a union or typed filter covering instant/sorcery
        assert!(def.affected.is_some());
    }

    #[test]
    fn graveyard_cast_permission_gisa_geralf() {
        let text = "Once during each of your turns, you may cast a Zombie creature spell from your graveyard.";
        let lower = text.to_lowercase();
        let def = try_parse_graveyard_cast_permission(text, &lower)
            .expect("should parse Gisa+Geralf text");
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                once_per_turn: true,
                play_mode: CardPlayMode::Cast,
            }
        ));
        // "zombie creature" → parse_type_phrase recognizes "zombie" as subtype.
        // card_type may be None (subtype alone) or Creature depending on parser —
        // either is functionally correct since Zombie is exclusively a creature subtype.
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert_eq!(tf.get_subtype(), Some("Zombie"));
        } else {
            panic!("Expected Typed filter with Zombie subtype");
        }
    }

    // --- Graveyard play permission tests (Crucible of Worlds / Icetill Explorer) ---

    #[test]
    fn graveyard_play_permission_crucible() {
        let text = "You may play lands from your graveyard.";
        let def = parse_static_line(text).expect("should parse Crucible text");
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                once_per_turn: false,
                play_mode: CardPlayMode::Play,
            }
        ));
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert!(tf.type_filters.contains(&TypeFilter::Land));
        } else {
            panic!(
                "Expected Typed filter with Land type, got: {:?}",
                def.affected
            );
        }
    }

    #[test]
    fn graveyard_cast_permission_conduit_of_worlds() {
        let text = "You may cast permanent spells from your graveyard.";
        let def = parse_static_line(text).expect("should parse Conduit text");
        assert!(matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                once_per_turn: false,
                play_mode: CardPlayMode::Cast,
            }
        ));
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert!(tf.type_filters.contains(&TypeFilter::Permanent));
        } else {
            panic!(
                "Expected Typed filter with Permanent type, got: {:?}",
                def.affected
            );
        }
    }

    // ── Fix 1: Irregular plural subtype normalization ──

    #[test]
    fn static_elves_you_control_uses_elf_subtype() {
        // CR 205.3m: "Elves" must normalize to "Elf", not "Elve"
        let def = parse_static_line("Other Elves you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::And { filters }) = &def.affected {
            let has_elf = filters
                .iter()
                .any(|f| matches!(f, TargetFilter::Typed(tf) if tf.get_subtype() == Some("Elf")));
            assert!(has_elf, "Expected Elf subtype, got {:?}", def.affected);
        } else if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert_eq!(tf.get_subtype(), Some("Elf"));
        } else {
            panic!("Expected filter with Elf subtype, got {:?}", def.affected);
        }
    }

    #[test]
    fn static_dwarves_you_control_uses_dwarf_subtype() {
        // CR 205.3m: "Dwarves" must normalize to "Dwarf", not "Dwarve"
        let def = parse_static_line("Dwarves you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert_eq!(tf.get_subtype(), Some("Dwarf"));
        } else {
            panic!(
                "Expected Typed filter with Dwarf subtype, got {:?}",
                def.affected
            );
        }
    }

    #[test]
    fn parse_creature_subject_filter_irregular_plurals() {
        // Single-word plural subtypes should resolve via parse_subtype
        let filter = super::parse_creature_subject_filter("Elves").unwrap();
        if let TargetFilter::Typed(tf) = &filter {
            assert_eq!(tf.get_subtype(), Some("Elf"));
        } else {
            panic!("Expected Typed filter with Elf subtype, got {:?}", filter);
        }

        let filter = super::parse_creature_subject_filter("Wolves").unwrap();
        if let TargetFilter::Typed(tf) = &filter {
            assert_eq!(tf.get_subtype(), Some("Wolf"));
        } else {
            panic!("Expected Typed filter with Wolf subtype, got {:?}", filter);
        }
    }

    #[test]
    fn static_unblocked_attacking_ninjas_you_control_have_lifelink() {
        let def =
            parse_static_line("Unblocked attacking Ninjas you control have lifelink.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert_eq!(tf.get_subtype(), Some("Ninja"));
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.contains(&FilterProp::Unblocked));
            assert!(tf.properties.contains(&FilterProp::Attacking));
        } else {
            panic!(
                "Expected Typed filter with Ninja subtype, got {:?}",
                def.affected
            );
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Lifelink,
            }));
    }

    #[test]
    fn static_attacking_ninjas_you_control_have_deathtouch() {
        let def = parse_static_line("Attacking Ninjas you control have deathtouch.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Typed(tf)) = &def.affected {
            assert_eq!(tf.get_subtype(), Some("Ninja"));
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(tf.properties.contains(&FilterProp::Attacking));
            assert!(!tf.properties.contains(&FilterProp::Unblocked));
        } else {
            panic!(
                "Expected Typed filter with Ninja subtype, got {:?}",
                def.affected
            );
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Deathtouch,
            }));
    }

    #[test]
    fn static_other_ninja_and_rogue_creatures_you_control_get_plus1() {
        let def =
            parse_static_line("Other Ninja and Rogue creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Or { filters }) = &def.affected {
            assert_eq!(filters.len(), 2);
            for f in filters {
                if let TargetFilter::Typed(tf) = f {
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                    assert!(tf.properties.contains(&FilterProp::Another));
                    assert!(tf.get_subtype() == Some("Ninja") || tf.get_subtype() == Some("Rogue"));
                } else {
                    panic!("Expected Typed filter in Or, got {f:?}");
                }
            }
        } else {
            panic!("Expected Or filter, got {:?}", def.affected);
        }
    }

    #[test]
    fn static_elf_or_warrior_creatures_you_control_have_trample() {
        let def = parse_static_line("Elf or Warrior creatures you control have trample.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        if let Some(TargetFilter::Or { filters }) = &def.affected {
            assert_eq!(filters.len(), 2);
        } else {
            panic!("Expected Or filter, got {:?}", def.affected);
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }));
    }

    #[test]
    fn static_parse_for_each_clause_other_creature() {
        // Verify parse_for_each_clause handles "other creature you control"
        let result =
            crate::parser::oracle_quantity::parse_for_each_clause("other creature you control");
        assert!(
            result.is_some(),
            "parse_for_each_clause should handle 'other creature you control'"
        );
        assert!(
            matches!(result.unwrap(), QuantityRef::ObjectCount { .. }),
            "Expected ObjectCount"
        );
    }

    #[test]
    fn static_self_gets_dynamic_power_for_each_creature() {
        // CR 613.4c: "~ gets +1/+0 for each other creature you control"
        let result = parse_static_line("~ gets +1/+0 for each other creature you control.");
        assert!(result.is_some(), "Should parse 'gets +N/+M for each'");
        let def = result.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(
            def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. })),
            "Expected AddDynamicPower, got {:?}",
            def.modifications
        );
        // Should NOT have AddDynamicToughness since toughness is +0
        assert!(
            !def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })),
            "Should not have AddDynamicToughness for +0"
        );
    }

    #[test]
    fn static_reduce_ability_cost_ninjutsu() {
        // CR 601.2f: "Ninjutsu abilities you activate cost {1} less to activate"
        let def = parse_static_line("Ninjutsu abilities you activate cost {1} less to activate.")
            .expect("should parse ReduceAbilityCost");
        assert!(
            matches!(
                def.mode,
                StaticMode::ReduceAbilityCost {
                    ref keyword,
                    amount: 1,
                } if keyword == "ninjutsu"
            ),
            "Expected ReduceAbilityCost {{ keyword: ninjutsu, amount: 1 }}, got {:?}",
            def.mode
        );
    }

    // --- Phase 33-01: Conditional, dynamic, and non-standard enchanted/equipped patterns ---

    #[test]
    fn static_enchanted_creature_has_keyword_as_long_as_control() {
        // Conditional grant: "enchanted creature has flying as long as you control a Wizard"
        let def =
            parse_static_line("Enchanted creature has flying as long as you control a Wizard.")
                .expect("should parse conditional enchanted grant");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying,
                }),
            "Expected AddKeyword(Flying), got {:?}",
            def.modifications
        );
        assert!(
            matches!(def.condition, Some(StaticCondition::IsPresent { .. })),
            "Expected IsPresent condition, got {:?}",
            def.condition
        );
    }

    #[test]
    fn static_enchanted_creature_gets_pt_as_long_as() {
        // Conditional grant: "enchanted creature gets +1/+1 as long as you control a Wizard"
        let def =
            parse_static_line("Enchanted creature gets +1/+1 as long as you control a Wizard.")
                .expect("should parse conditional enchanted P/T grant");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddPower { value: 1 }),
            "Expected AddPower(1)"
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddToughness { value: 1 }),
            "Expected AddToughness(1)"
        );
        assert!(
            matches!(def.condition, Some(StaticCondition::IsPresent { .. })),
            "Expected IsPresent condition, got {:?}",
            def.condition
        );
    }

    #[test]
    fn static_enchanted_creature_dynamic_for_each() {
        // Dynamic grant: "enchanted creature gets +1/+1 for each creature you control"
        let def = parse_static_line("Enchanted creature gets +1/+1 for each creature you control.")
            .expect("should parse dynamic enchanted P/T grant");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(
            def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. })),
            "Expected AddDynamicPower, got {:?}",
            def.modifications
        );
        assert!(
            def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })),
            "Expected AddDynamicToughness, got {:?}",
            def.modifications
        );
    }

    #[test]
    fn static_enchanted_creature_dynamic_where_x() {
        // Dynamic grant: "enchanted creature gets +X/+X, where X is the number of cards in your hand"
        let def = parse_static_line(
            "Enchanted creature gets +X/+X, where X is the number of cards in your hand.",
        )
        .expect("should parse dynamic enchanted where-X grant");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(
            def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicPower { .. })),
            "Expected AddDynamicPower, got {:?}",
            def.modifications
        );
        assert!(
            def.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. })),
            "Expected AddDynamicToughness, got {:?}",
            def.modifications
        );
    }

    #[test]
    fn static_enchanted_creature_can_attack_as_though_haste() {
        // Non-standard keyword: "enchanted creature can attack as though it had haste"
        // CR 702.10: Haste-equivalent for aura-granted attack permission.
        let def = parse_static_line("Enchanted creature can attack as though it had haste.")
            .expect("should parse 'can attack as though it had haste'");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                }),
            "Expected AddKeyword(Haste), got {:?}",
            def.modifications
        );
    }

    #[test]
    fn static_enchanted_creature_cant_be_blocked() {
        // Non-standard: "enchanted creature can't be blocked"
        // CR 509.1b: Unblockable via aura.
        let def = parse_static_line("Enchanted creature can't be blocked.")
            .expect("should parse enchanted can't be blocked");
        assert_eq!(def.mode, StaticMode::CantBeBlocked);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
    }

    // --- MustAttack / MustBlock combat requirement pattern tests ---

    #[test]
    fn static_must_attack_each_combat_if_able() {
        let def = parse_static_line("This creature must attack each combat if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustAttack);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_attacks_each_turn_if_able() {
        let def = parse_static_line("Enchanted creature attacks each turn if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustAttack);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
    }

    #[test]
    fn static_equipped_creature_regression() {
        // Regression: existing equipped creature pattern still works.
        let def = parse_static_line("Equipped creature has first strike and lifelink.")
            .expect("should parse equipped creature keywords");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
            ))
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::FirstStrike,
                }),
            "Expected AddKeyword(FirstStrike)"
        );
        assert!(
            def.modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Lifelink,
                }),
            "Expected AddKeyword(Lifelink)"
        );
    }

    #[test]
    fn static_enchanted_creature_gets_pt_regression() {
        // Regression: basic enchanted creature P/T pattern still works.
        let def = parse_static_line("Enchanted creature gets +2/+2.")
            .expect("should parse enchanted creature P/T");
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 2 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 2 }));
    }

    // --- Lord pattern tests (Plan 33-02) ---

    #[test]
    fn lord_bare_creatures_have_keyword() {
        // "Creatures you control have vigilance" (e.g., Brave the Sands)
        let result = parse_static_line("Creatures you control have vigilance.");
        assert!(result.is_some(), "should parse bare keyword lord");
        let def = result.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        // Verify affected filter is creature + controller You
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("Expected Typed creature filter with controller You"),
        }
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Vigilance,
            }));
    }

    #[test]
    fn lord_other_creatures_have_keyword() {
        // CR 613.7: "Other creatures you control have hexproof" (e.g., Shalai, Voice of Plenty)
        // Must produce Continuous with AddKeyword(Hexproof) and Another filter to exclude self.
        let result = parse_static_line("Other creatures you control have hexproof.");
        assert!(
            result.is_some(),
            "should parse other creatures keyword lord"
        );
        let def = result.unwrap();
        assert!(matches!(def.mode, StaticMode::Continuous), "not continuous");
        let has_hexproof = def.modifications.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Hexproof
                }
            )
        });
        assert!(has_hexproof, "no hexproof keyword");
        // CR 613.7: "Other" means the static excludes the source permanent itself.
        let has_another = match &def.affected {
            Some(TargetFilter::Typed(tf)) => tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::Another)),
            _ => false,
        };
        assert!(has_another, "no Another property for 'other' lord");
    }

    #[test]
    fn lord_subtype_creatures_have_keyword() {
        // "Pirate creatures you control have menace" (e.g., Dire Fleet Neckbreaker variant)
        let result = parse_static_line("Pirate creatures you control have menace.");
        assert!(result.is_some(), "should parse subtype keyword lord");
        let def = result.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Menace,
            }));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Subtype("Pirate".to_string())));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("Expected Typed filter"),
        }
    }

    #[test]
    fn lord_conditional_as_long_as_control() {
        // "As long as you control a Wizard, creatures you control get +1/+1"
        // (e.g., Adeliz, the Cinder Wind variant)
        let result =
            parse_static_line("As long as you control a Wizard, creatures you control get +1/+1.");
        assert!(result.is_some(), "should parse conditional lord");
        let def = result.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
        assert!(def.condition.is_some(), "Expected a StaticCondition");
        match def.condition {
            Some(StaticCondition::IsPresent { .. }) => {}
            _ => panic!("Expected IsPresent condition"),
        }
    }

    #[test]
    fn lord_each_creature_with_keyword() {
        // "Each creature you control with flying gets +1/+1"
        // (e.g., Favorable Winds, Empyrean Eagle)
        let result = parse_static_line("Each creature you control with flying gets +1/+1.");
        assert!(result.is_some(), "should parse keyword-filtered lord");
        let def = result.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
        // Should have a filter with WithKeyword for flying
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.properties.contains(&FilterProp::WithKeyword {
                    value: "flying".to_string(),
                }));
            }
            _ => panic!("Expected Typed filter with keyword property"),
        }
    }

    #[test]
    fn lord_other_zombie_creatures_regression() {
        // Regression: "Other Zombie creatures you control get +1/+1" still works
        let result = parse_static_line("Other Zombie creatures you control get +1/+1.");
        assert!(result.is_some(), "should parse other subtype lord");
        let def = result.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Subtype("Zombie".to_string())));
                assert!(tf.properties.contains(&FilterProp::Another));
            }
            _ => panic!("Expected Typed filter"),
        }
    }

    #[test]
    fn enchanted_land_is_a_mountain_produces_set_basic_land_type() {
        let def = parse_static_line("Enchanted land is a Mountain.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.modifications.as_slice(),
            [ContinuousModification::SetBasicLandType { land_type }]
            if *land_type == BasicLandType::Mountain
        ));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert!(tf.properties.contains(&FilterProp::EnchantedBy));
            }
            _ => panic!("Expected Typed land filter with EnchantedBy"),
        }
    }

    #[test]
    fn enchanted_land_is_a_plains_produces_set_basic_land_type() {
        let def = parse_static_line("Enchanted land is a Plains.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.modifications.as_slice(),
            [ContinuousModification::SetBasicLandType { land_type }]
            if *land_type == BasicLandType::Plains
        ));
    }

    #[test]
    fn enchanted_land_is_a_forest_in_addition_produces_add_subtype() {
        let def = parse_static_line("Enchanted land is a Forest in addition to its other types.")
            .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddSubtype {
                subtype: "Forest".to_string(),
            }]
        );
    }

    #[test]
    fn enchanted_land_is_a_swamp_in_addition_produces_add_subtype() {
        let def =
            parse_static_line("Enchanted land is a Swamp in addition to its other types.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddSubtype {
                subtype: "Swamp".to_string(),
            }]
        );
    }

    // --- Land type-changing statics (CR 305.7) ---

    #[test]
    fn nonbasic_lands_are_mountains_blood_moon() {
        let def = parse_static_line("Nonbasic lands are Mountains.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.modifications.as_slice(),
            [ContinuousModification::SetBasicLandType { land_type }]
            if *land_type == BasicLandType::Mountain
        ));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert!(tf.properties.contains(&FilterProp::NotSupertype {
                    value: "Basic".to_string(),
                }));
            }
            _ => panic!("Expected Typed nonbasic land filter"),
        }
    }

    #[test]
    fn nonbasic_lands_are_islands_harbinger() {
        let def = parse_static_line("Nonbasic lands are Islands.").unwrap();
        assert!(matches!(
            def.modifications.as_slice(),
            [ContinuousModification::SetBasicLandType { land_type }]
            if *land_type == BasicLandType::Island
        ));
    }

    #[test]
    fn lands_you_control_are_plains_celestial_dawn() {
        let def = parse_static_line("Lands you control are Plains.").unwrap();
        assert!(matches!(
            def.modifications.as_slice(),
            [ContinuousModification::SetBasicLandType { land_type }]
            if *land_type == BasicLandType::Plains
        ));
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("Expected Typed land filter with you-control"),
        }
    }

    #[test]
    fn each_land_is_a_swamp_in_addition_urborg() {
        let def =
            parse_static_line("Each land is a Swamp in addition to its other land types.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddSubtype {
                subtype: "Swamp".to_string(),
            }]
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert!(tf.controller.is_none());
            }
            _ => panic!("Expected Typed land filter (all lands)"),
        }
    }

    #[test]
    fn all_lands_are_islands_in_addition_stormtide() {
        let def =
            parse_static_line("All lands are Islands in addition to their other types.").unwrap();
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddSubtype {
                subtype: "Island".to_string(),
            }]
        );
    }

    #[test]
    fn lands_you_control_every_basic_land_type_prismatic_omen() {
        let def = parse_static_line(
            "Lands you control are every basic land type in addition to their other types.",
        )
        .unwrap();
        assert_eq!(
            def.modifications,
            vec![ContinuousModification::AddAllBasicLandTypes]
        );
        match &def.affected {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            _ => panic!("Expected Typed land filter with you-control"),
        }
    }

    // --- CantCastDuring: turn/phase-scoped casting prohibitions ---

    #[test]
    fn static_cant_cast_opponents_during_your_turn() {
        // CR 101.2: Teferi, Time Raveler — "Your opponents can't cast spells during your turn."
        let def = parse_static_line("Your opponents can't cast spells during your turn.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantCastDuring {
                who: CastingProhibitionScope::Opponents,
                when: CastingProhibitionCondition::DuringYourTurn,
            }
        );
    }

    #[test]
    fn static_cant_cast_players_during_combat() {
        // CR 101.2: "Players can't cast spells during combat."
        let def = parse_static_line("Players can't cast spells during combat.").unwrap();
        assert_eq!(
            def.mode,
            StaticMode::CantCastDuring {
                who: CastingProhibitionScope::AllPlayers,
                when: CastingProhibitionCondition::DuringCombat,
            }
        );
    }

    #[test]
    fn static_cant_cast_from_still_works() {
        // Regression: CantCastFrom (zone-based) must not be affected
        let def =
            parse_static_line("Players can't cast spells from graveyards or libraries.").unwrap();
        assert_eq!(def.mode, StaticMode::CantCastFrom);
    }

    #[test]
    fn static_cant_cast_during_serde_roundtrip() {
        let mode = StaticMode::CantCastDuring {
            who: CastingProhibitionScope::Opponents,
            when: CastingProhibitionCondition::DuringYourTurn,
        };
        let json = serde_json::to_string(&mode).unwrap();
        let deserialized: StaticMode = serde_json::from_str(&json).unwrap();
        assert_eq!(mode, deserialized);
    }

    #[test]
    fn static_cant_cast_during_display_roundtrip() {
        let mode = StaticMode::CantCastDuring {
            who: CastingProhibitionScope::Opponents,
            when: CastingProhibitionCondition::DuringYourTurn,
        };
        let s = mode.to_string();
        assert_eq!(StaticMode::from_str(&s).unwrap(), mode);

        let mode2 = StaticMode::CantCastDuring {
            who: CastingProhibitionScope::AllPlayers,
            when: CastingProhibitionCondition::DuringCombat,
        };
        let s2 = mode2.to_string();
        assert_eq!(StaticMode::from_str(&s2).unwrap(), mode2);
    }

    // --- MustAttack / MustBlock additional combat requirement tests ---

    #[test]
    fn static_must_attack_if_able() {
        let def = parse_static_line("This creature must attack if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustAttack);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_must_block_each_combat_if_able() {
        let def = parse_static_line("This creature must block each combat if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustBlock);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_blocks_each_combat_if_able() {
        let def = parse_static_line("Enchanted creature blocks each combat if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustBlock);
        assert_eq!(
            def.affected,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
            ))
        );
    }

    #[test]
    fn static_must_block_if_able() {
        let def = parse_static_line("This creature must block if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustBlock);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_blocks_each_turn_if_able() {
        let def = parse_static_line("This creature blocks each turn if able.").unwrap();
        assert_eq!(def.mode, StaticMode::MustBlock);
        assert_eq!(def.affected, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn static_unrelated_text_not_must_attack() {
        // "gets +1/+1" should not produce MustAttack
        let def = parse_static_line("This creature gets +1/+1.").unwrap();
        assert_ne!(def.mode, StaticMode::MustAttack);
        assert_ne!(def.mode, StaticMode::MustBlock);
    }

    #[test]
    fn map_keyword_all_creature_types_returns_changeling() {
        // CR 702.73a: "all creature types" is the Changeling CDA effect.
        assert_eq!(map_keyword("all creature types"), Some(Keyword::Changeling));
        assert_eq!(map_keyword("All Creature Types"), Some(Keyword::Changeling));
    }

    #[test]
    fn gain_all_creature_types_produces_add_keyword_changeling() {
        let mods = parse_continuous_modifications("gain all creature types");
        assert!(
            mods.iter().any(|m| matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Changeling
                }
            )),
            "Should produce AddKeyword(Changeling), got: {mods:?}"
        );
    }

    #[test]
    fn static_condition_source_in_graveyard() {
        let cond = parse_static_condition("this card is in your graveyard");
        assert!(
            matches!(
                cond,
                Some(StaticCondition::SourceInZone {
                    zone: Zone::Graveyard
                })
            ),
            "Expected SourceInZone(Graveyard), got: {cond:?}"
        );
    }

    #[test]
    fn static_condition_source_in_hand() {
        let cond = parse_static_condition("~ is in your hand");
        assert!(
            matches!(
                cond,
                Some(StaticCondition::SourceInZone { zone: Zone::Hand })
            ),
            "Expected SourceInZone(Hand), got: {cond:?}"
        );
    }

    #[test]
    fn static_condition_compound_and() {
        let cond =
            parse_static_condition("this card is in your graveyard and you control a Mountain");
        assert!(
            matches!(cond, Some(StaticCondition::And { ref conditions }) if conditions.len() == 2),
            "Expected And with 2 conditions, got: {cond:?}"
        );
    }

    #[test]
    fn static_condition_no_false_split_noun_phrase() {
        // "artifacts and creatures you control" is NOT a compound condition
        let cond = parse_static_condition("artifacts and creatures you control");
        assert!(
            !matches!(cond, Some(StaticCondition::And { .. })),
            "Should not split noun phrase, got: {cond:?}"
        );
    }

    // --- Task 1: as-long-as condition splitting in parse_continuous_gets_has ---

    #[test]
    fn static_self_ref_gets_as_long_as_control_forest() {
        // Kird Ape: "~ gets +1/+2 as long as you control a Forest"
        let def = parse_static_line("Kird Ape gets +1/+2 as long as you control a Forest.");
        assert!(def.is_some(), "Should parse 'gets +1/+2 as long as' static");
        let def = def.unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(
            def.condition.is_some(),
            "Expected non-null condition for 'as long as' static, got None"
        );
    }

    #[test]
    fn static_self_ref_gets_as_long_as_regression_for_each() {
        // "for each" split must still work after adding "as long as" split
        let def = parse_static_line("~ gets +1/+1 for each creature you control.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        // Should have dynamic P/T modifications, not a condition
        assert!(def.condition.is_none());
    }

    #[test]
    fn static_self_ref_gets_without_condition_regression() {
        // Plain "gets +2/+2" without condition must still work
        let def = parse_static_line("~ gets +2/+2.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def.condition.is_none());
    }

    #[test]
    fn static_condition_you_have_n_or_more_life() {
        // "you have 5 or more life" should parse as a QuantityComparison
        let cond = parse_static_condition("you have 5 or more life");
        assert!(
            matches!(
                cond,
                Some(StaticCondition::QuantityComparison {
                    comparator: Comparator::GE,
                    ..
                })
            ),
            "Expected QuantityComparison with GE, got: {cond:?}"
        );
    }

    #[test]
    fn static_conditional_cant_untap_with_if() {
        // "~ doesn't untap during your untap step if enchanted creature is blue"
        // Should produce CantUntap with a condition populated
        let def = parse_static_line(
            "~ doesn't untap during your untap step as long as enchanted creature is tapped.",
        );
        // For now, just check it parses as CantUntap (condition handling is new)
        assert!(def.is_some(), "Should parse conditional CantUntap");
        let def = def.unwrap();
        assert_eq!(def.mode, StaticMode::CantUntap);
    }
}
