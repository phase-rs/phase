use std::str::FromStr;

use super::oracle_util::strip_reminder_text;
use crate::types::ability::{
    ContinuousModification, ControllerRef, FilterProp, StaticCondition, StaticDefinition,
    TargetFilter, TypeFilter,
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
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::EnchantedBy],
            },
        );
    }

    // --- "Enchanted permanent gets/has ..." ---
    if lower.starts_with("enchanted permanent ") {
        return parse_continuous_gets_has(
            &text[20..],
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Permanent),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::EnchantedBy],
            },
        );
    }

    // --- "Equipped creature gets +N/+M" ---
    if lower.starts_with("equipped creature ") {
        return parse_continuous_gets_has(
            &text[18..],
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::EquippedBy],
            },
        );
    }

    // --- "All creatures get/have ..." ---
    if lower.starts_with("all creatures ") {
        return parse_continuous_gets_has(
            &text[14..],
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![],
            },
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
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties: vec![],
            },
        );
    }

    // --- "Other creatures you control get +N/+M" ---
    if lower.starts_with("other creatures you control ") {
        return parse_continuous_gets_has(
            &text[28..],
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties: vec![],
            },
        );
    }

    // --- "Lands you control have '[type]'" ---
    if lower.starts_with("lands you control have ") {
        let rest = text[23..]
            .trim()
            .trim_end_matches('.')
            .trim_matches(|c: char| c == '\'' || c == '"');
        return Some(StaticDefinition {
            mode: StaticMode::Continuous,
            affected: Some(TargetFilter::Typed {
                card_type: Some(TypeFilter::Land),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties: vec![],
            }),
            modifications: vec![ContinuousModification::AddSubtype {
                subtype: rest.to_string(),
            }],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
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
                return Some(StaticDefinition {
                    mode: StaticMode::Continuous,
                    affected: Some(TargetFilter::SelfRef),
                    modifications,
                    condition: Some(StaticCondition::DuringYourTurn),
                    affected_zone: None,
                    effect_zone: None,
                    characteristic_defining: false,
                    description: Some(text.to_string()),
                });
            }
        }
        // Fallback: "during your turn, ~ gets +N/+M"
        if let Some(gets_pos) = rest.find(" gets ") {
            let mods_text = &original_rest[gets_pos + 6..];
            let modifications = parse_continuous_modifications(mods_text);
            if !modifications.is_empty() {
                return Some(StaticDefinition {
                    mode: StaticMode::Continuous,
                    affected: Some(TargetFilter::SelfRef),
                    modifications,
                    condition: Some(StaticCondition::DuringYourTurn),
                    affected_zone: None,
                    effect_zone: None,
                    characteristic_defining: false,
                    description: Some(text.to_string()),
                });
            }
        }
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
                return Some(StaticDefinition {
                    mode: StaticMode::Continuous,
                    affected: Some(TargetFilter::SelfRef),
                    modifications,
                    condition: Some(StaticCondition::CheckSVar {
                        var: "condition".to_string(),
                        compare: condition_text.to_string(),
                    }),
                    affected_zone: None,
                    effect_zone: None,
                    characteristic_defining: false,
                    description: Some(text.to_string()),
                });
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
        return Some(StaticDefinition {
            mode: StaticMode::Other("CantBeBlocked".to_string()),
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
    }

    // --- "~ can't block" ---
    if lower.contains("can't block") && !lower.contains("can't be blocked") {
        return Some(StaticDefinition {
            mode: StaticMode::CantBlock,
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
    }

    // --- "~ can't attack" ---
    if lower.contains("can't attack") {
        return Some(StaticDefinition {
            mode: StaticMode::CantAttack,
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
    }

    // --- "~ can't be countered" ---
    if lower.contains("can't be countered") {
        return Some(StaticDefinition {
            mode: StaticMode::CantBeCast,
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
    }

    // --- "~ can't be the target" or "~ can't be targeted" ---
    if lower.contains("can't be the target") || lower.contains("can't be targeted") {
        return Some(StaticDefinition {
            mode: StaticMode::CantBeTargeted,
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
    }

    // --- "~ can't be sacrificed" ---
    if lower.contains("can't be sacrificed") {
        return Some(StaticDefinition {
            mode: StaticMode::Continuous,
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
    }

    // --- "~ doesn't untap during your untap step" ---
    if lower.contains("doesn't untap during") || lower.contains("doesn\u{2019}t untap during") {
        return Some(StaticDefinition {
            mode: StaticMode::Continuous,
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
    }

    // --- "~ enters with N [type] counters" or "~ enters the battlefield with N [type] counters" ---
    if lower.contains("enters") && lower.contains("counter") {
        if let Some(_etb) = parse_etb_counters(&lower) {
            return Some(StaticDefinition {
                mode: StaticMode::Continuous,
                affected: Some(TargetFilter::SelfRef),
                modifications: vec![],
                condition: None,
                affected_zone: None,
                effect_zone: None,
                characteristic_defining: false,
                description: Some(text.to_string()),
            });
        }
        // Even if we can't parse the exact count, capture it
        return Some(StaticDefinition {
            mode: StaticMode::Continuous,
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
    }

    // --- "Spells you cast cost {N} less" ---
    if lower.contains("cost") && lower.contains("less") && lower.contains("spell") {
        return Some(StaticDefinition {
            mode: StaticMode::ReduceCost,
            affected: Some(TargetFilter::Typed {
                card_type: Some(TypeFilter::Card),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties: vec![],
            }),
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
    }

    // --- "Spells your opponents cast cost {N} more" ---
    if lower.contains("cost")
        && lower.contains("more")
        && lower.contains("spell")
        && lower.contains("opponent")
    {
        return Some(StaticDefinition {
            mode: StaticMode::RaiseCost,
            affected: Some(TargetFilter::Typed {
                card_type: Some(TypeFilter::Card),
                subtype: None,
                controller: Some(ControllerRef::Opponent),
                properties: vec![],
            }),
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
    }

    // --- "As long as ..." (generic conditional static) ---
    if lower.starts_with("as long as ") {
        return Some(StaticDefinition {
            mode: StaticMode::Continuous,
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![],
            condition: Some(StaticCondition::CheckSVar {
                var: "condition".to_string(),
                compare: text.trim_end_matches('.').to_string(),
            }),
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
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
        let subtype_str = &text[..creatures_pos];
        if !subtype_str.trim().is_empty() && is_capitalized_words(subtype_str) {
            let after_prefix = &text[creatures_pos + 23..];
            let filter = TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: Some(subtype_str.to_string()),
                controller: Some(ControllerRef::You),
                properties: vec![],
            };
            return parse_continuous_gets_has(after_prefix, filter);
        }
    }

    // Try "Xs you control get/have" (e.g. "Zombies you control get +1/+1")
    if let Some(yc_pos) = lower.find(" you control ") {
        let subtype_str = &text[..yc_pos];
        if !subtype_str.trim().is_empty() && is_capitalized_words(subtype_str) {
            // Strip trailing 's' for the subtype name (Zombies -> Zombie)
            let subtype_name = subtype_str.trim_end_matches('s').to_string();
            let after_prefix = &text[yc_pos + 13..];
            let filter = TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: Some(subtype_name),
                controller: Some(ControllerRef::You),
                properties: vec![],
            };
            return parse_continuous_gets_has(after_prefix, filter);
        }
    }

    None
}

/// Check that a string is one or more capitalized words.
fn is_capitalized_words(s: &str) -> bool {
    let trimmed = s.trim();
    !trimmed.is_empty()
        && trimmed
            .split_whitespace()
            .all(|w| w.chars().next().map_or(false, |c| c.is_uppercase()))
}

/// Parse "gets +N/+M [and has {keyword}]" after the subject.
fn parse_continuous_gets_has(text: &str, affected: TargetFilter) -> Option<StaticDefinition> {
    let modifications = parse_continuous_modifications(text);

    if modifications.is_empty() {
        return None;
    }

    Some(StaticDefinition {
        mode: StaticMode::Continuous,
        affected: Some(affected),
        modifications,
        condition: None,
        affected_zone: None,
        effect_zone: None,
        characteristic_defining: false,
        description: None,
    })
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

/// Parse "enters with N [type] counters" pattern, returning (count, counter_type).
fn parse_etb_counters(lower: &str) -> Option<(u32, String)> {
    // Look for pattern: "with N type counter(s)"
    let with_pos = lower.find("with ")?;
    let after_with = &lower[with_pos + 5..];
    let mut words = after_with.split_whitespace();
    let count_str = words.next()?;
    let count: u32 = count_str.parse().ok()?;
    let counter_type = words.next()?;
    Some((count, counter_type.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Some(TargetFilter::Typed {
                controller: Some(ControllerRef::You),
                ..
            })
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
            Some(TargetFilter::Typed {
                card_type: Some(TypeFilter::Permanent),
                ..
            })
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
            Some(TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                controller: None,
                ..
            })
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
            Some(TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: Some(ref s),
                controller: Some(ControllerRef::You),
                ..
            }) if s == "Elf"
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

    #[test]
    fn static_enters_with_counters() {
        let def = parse_static_line(
            "Polukranos enters the battlefield with twelve +1/+1 counters on it.",
        )
        .unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def.description.is_some());
    }

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
}
