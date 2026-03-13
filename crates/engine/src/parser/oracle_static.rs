use std::str::FromStr;

use super::oracle_util::strip_reminder_text;
use crate::types::ability::{
    ChosenSubtypeKind, ContinuousModification, ControllerRef, DynamicPTValue, FilterProp,
    StaticCondition, StaticDefinition, TargetFilter, TypedFilter,
};
use crate::types::keywords::Keyword;
use crate::types::statics::StaticMode;

/// Parse a static/continuous ability line into a StaticDefinition.
/// Handles: "Enchanted creature gets +N/+M", "has {keyword}",
/// "Creatures you control get +N/+M", etc.
pub fn parse_static_line(text: &str) -> Option<StaticDefinition> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();

    // --- "Enchanted creature gets +N/+M" or "has {keyword}" ---
    if lower.starts_with("enchanted creature ") {
        return parse_continuous_gets_has(
            &text[19..],
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])),
        );
    }

    // --- "Enchanted permanent gets/has ..." ---
    if lower.starts_with("enchanted permanent ") {
        return parse_continuous_gets_has(
            &text[20..],
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy])),
        );
    }

    // --- "Equipped creature gets +N/+M" ---
    if lower.starts_with("equipped creature ") {
        return parse_continuous_gets_has(
            &text[18..],
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy])),
        );
    }

    // --- "All creatures get/have ..." ---
    if lower.starts_with("all creatures ") {
        return parse_continuous_gets_has(
            &text[14..],
            TargetFilter::Typed(TypedFilter::creature()),
        );
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

    // --- "Creatures you control get +N/+M" ---
    if lower.starts_with("creatures you control ") {
        return parse_continuous_gets_has(
            &text[22..],
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        );
    }

    // --- "Other creatures you control get +N/+M" ---
    if lower.starts_with("other creatures you control ") {
        return parse_continuous_gets_has(
            &text[28..],
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        );
    }

    // --- "Lands you control have '[type]'" ---
    if lower.starts_with("lands you control have ") {
        let rest = text[23..]
            .trim()
            .trim_end_matches('.')
            .trim_matches(|c: char| c == '\'' || c == '"');
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You)))
                .modifications(vec![ContinuousModification::AddSubtype {
                    subtype: rest.to_string(),
                }])
                .description(text.to_string()),
        );
    }

    // --- "During your turn, ~ has [keyword]" ---
    if let Some(rest) = lower.strip_prefix("during your turn, ") {
        let original_rest = &text["during your turn, ".len()..];
        // Expect "~ has {keyword}" or "this creature has {keyword}"
        if let Some(has_pos) = rest.find(" has ") {
            let keyword_text = rest[has_pos + 5..].trim().trim_end_matches('.');
            let mut modifications = Vec::new();
            for kw_part in keyword_text.split(" and ") {
                let kw_part = kw_part.trim().trim_end_matches('.');
                if let Some(kw) = map_keyword(kw_part) {
                    modifications.push(ContinuousModification::AddKeyword { keyword: kw });
                }
            }
            if !modifications.is_empty() {
                return Some(
                    StaticDefinition::continuous()
                        .affected(TargetFilter::SelfRef)
                        .modifications(modifications)
                        .condition(StaticCondition::DuringYourTurn)
                        .description(text.to_string()),
                );
            }
        }
        // Fallback: "during your turn, ~ gets +N/+M"
        if let Some(gets_pos) = rest.find(" gets ") {
            let mods_text = &original_rest[gets_pos + 6..];
            let modifications = parse_continuous_modifications(mods_text);
            if !modifications.is_empty() {
                return Some(
                    StaticDefinition::continuous()
                        .affected(TargetFilter::SelfRef)
                        .modifications(modifications)
                        .condition(StaticCondition::DuringYourTurn)
                        .description(text.to_string()),
                );
            }
        }
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
                return Some(
                    StaticDefinition::continuous()
                        .affected(TargetFilter::SelfRef)
                        .modifications(modifications)
                        .condition(StaticCondition::CheckSVar {
                            var: "condition".to_string(),
                            compare: condition_text.to_string(),
                        })
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
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // NOTE: "enters with N counters" patterns are now handled by oracle_replacement.rs
    // as proper Moved replacement effects (paralleling the "enters tapped" pattern).

    // --- "Spells you cast cost {N} less" ---
    if lower.contains("cost") && lower.contains("less") && lower.contains("spell") {
        return Some(
            StaticDefinition::new(StaticMode::ReduceCost)
                .affected(TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::You)))
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
                .affected(TargetFilter::Typed(TypedFilter::card().controller(ControllerRef::Opponent)))
                .description(text.to_string()),
        );
    }

    // --- "As long as ..." (generic conditional static) ---
    if lower.starts_with("as long as ") {
        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .condition(StaticCondition::CheckSVar {
                    var: "condition".to_string(),
                    compare: text.trim_end_matches('.').to_string(),
                })
                .description(text.to_string()),
        );
    }

    None
}

/// Try to parse "[Subtype] creatures you control get/have ..." patterns.
/// `text` is the original-case text starting at the subtype word.
/// `lower` is the lowercased version of `text`.
/// `is_other` indicates whether this was preceded by "Other ".
fn parse_typed_you_control(text: &str, lower: &str, _is_other: bool) -> Option<StaticDefinition> {
    // Try "X creatures you control get/have" first
    if let Some(creatures_pos) = lower.find(" creatures you control ") {
        let descriptor = text[..creatures_pos].trim();
        if !descriptor.is_empty() {
            let after_prefix = &text[creatures_pos + 23..];
            let typed_filter = if let Some(color) = parse_named_color(descriptor) {
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::HasColor { color: color.to_string() }]),
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
            return parse_continuous_gets_has(after_prefix, typed_filter);
        }
    }

    // Try "Xs you control get/have" (e.g. "Zombies you control get +1/+1")
    if let Some(yc_pos) = lower.find(" you control ") {
        let descriptor = text[..yc_pos].trim();
        if !descriptor.is_empty() {
            let after_prefix = &text[yc_pos + 13..];
            let typed_filter = if let Some(color) = parse_named_color(descriptor) {
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::HasColor { color: color.to_string() }]),
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
            return parse_continuous_gets_has(after_prefix, typed_filter);
        }
    }

    None
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
fn parse_continuous_gets_has(text: &str, affected: TargetFilter) -> Option<StaticDefinition> {
    let modifications = parse_continuous_modifications(text);

    if modifications.is_empty() {
        return None;
    }

    Some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(modifications),
    )
}

pub(crate) fn parse_continuous_modifications(text: &str) -> Vec<ContinuousModification> {
    let lower = text.to_lowercase();
    let mut modifications = Vec::new();

    if lower.starts_with("gets ") || lower.starts_with("get ") {
        let offset = if lower.starts_with("gets ") { 5 } else { 4 };
        let after = &text[offset..].trim();
        if let Some((p, t)) = parse_pt_mod(after) {
            modifications.push(ContinuousModification::AddPower { value: p });
            modifications.push(ContinuousModification::AddToughness { value: t });
        }
    }

    if let Some(keyword_text) = extract_keyword_clause(text) {
        for part in split_keyword_list(keyword_text.trim().trim_end_matches('.')) {
            if let Some(kw) = map_keyword(part.trim().trim_end_matches('.')) {
                modifications.push(ContinuousModification::AddKeyword { keyword: kw });
            }
        }
    }

    modifications
}

/// Split a keyword list like "flying and trample" or "flying, trample, and haste".
fn split_keyword_list(text: &str) -> Vec<&str> {
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
    parts
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
    match Keyword::from_str(word) {
        Ok(Keyword::Unknown(_)) => None,
        Ok(kw) => Some(kw),
        Err(_) => None, // Infallible, but satisfy the compiler
    }
}

/// Parse CDA power/toughness equality patterns like:
/// - "~'s power is equal to the number of card types among cards in all graveyards
///   and its toughness is equal to that number plus 1."
fn parse_cda_pt_equality(lower: &str, text: &str) -> Option<StaticDefinition> {
    // Match "power is equal to the number of card types among cards in all graveyards"
    if !lower.contains("power is equal to")
        && !lower.contains("power and toughness are each equal to")
    {
        return None;
    }

    if lower.contains("card types among cards in all graveyards") {
        let mut modifications = vec![ContinuousModification::SetDynamicPower {
            value: DynamicPTValue::CardTypesInAllGraveyards { offset: 0 },
        }];

        // Check for "and its toughness is equal to that number plus N"
        if let Some(plus_pos) = lower.find("that number plus ") {
            let after_plus = &lower[plus_pos + 17..];
            let n_str = after_plus
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap_or("0");
            let offset = n_str.parse::<i32>().unwrap_or(0);
            modifications.push(ContinuousModification::SetDynamicToughness {
                value: DynamicPTValue::CardTypesInAllGraveyards { offset },
            });
        } else if lower.contains("power and toughness are each equal to") {
            // Same value for both
            modifications.push(ContinuousModification::SetDynamicToughness {
                value: DynamicPTValue::CardTypesInAllGraveyards { offset: 0 },
            });
        }

        return Some(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(modifications)
                .cda()
                .description(text.to_string()),
        );
    }

    None
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
        assert_eq!(def.mode, StaticMode::Continuous);
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
    fn static_as_long_as() {
        let def = parse_static_line(
            "As long as you control a creature with power 4 or greater, Elemental Bond has hexproof.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(
            def.condition,
            Some(StaticCondition::CheckSVar { .. })
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
            Some(StaticCondition::CheckSVar { .. })
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
                value: DynamicPTValue::CardTypesInAllGraveyards { offset: 0 },
            }));
        assert!(def
            .modifications
            .contains(&ContinuousModification::SetDynamicToughness {
                value: DynamicPTValue::CardTypesInAllGraveyards { offset: 1 },
            }));
    }
}
