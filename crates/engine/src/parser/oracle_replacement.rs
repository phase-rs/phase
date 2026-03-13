use super::oracle_effect::parse_effect_chain;
use super::oracle_util::strip_reminder_text;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementCondition, ReplacementDefinition,
    ReplacementMode, TargetFilter,
};
use crate::types::replacements::ReplacementEvent;

/// Parse a replacement effect line into a ReplacementDefinition.
/// Handles: "If ~ would die", "Prevent all combat damage",
/// "~ enters the battlefield tapped", etc.
pub fn parse_replacement_line(text: &str, card_name: &str) -> Option<ReplacementDefinition> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();
    let normalized = replace_self_refs(&text, card_name);
    let norm_lower = normalized.to_lowercase();

    // --- Shock lands: "As ~ enters, you may pay N life. If you don't, it enters tapped." ---
    // Must be checked BEFORE the generic "enters tapped" pattern.
    if let Some(def) = parse_shock_land(&norm_lower, &text) {
        return Some(def);
    }

    // --- Check lands: "enters tapped unless you control a [LandType] or a [LandType]" ---
    // Must be checked BEFORE the generic "enters tapped" pattern.
    if let Some(def) = parse_check_land(&norm_lower, &text) {
        return Some(def);
    }

    // --- "~ enters the battlefield tapped" (unconditional) ---
    if norm_lower.contains("enters the battlefield tapped") || norm_lower.contains("enters tapped")
    {
        let tap_effect = Box::new(AbilityDefinition {
            kind: AbilityKind::Spell,
            effect: Effect::Tap {
                target: TargetFilter::SelfRef,
            },
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
            condition: None,
        });
        return Some(ReplacementDefinition {
            execute: Some(tap_effect),
            valid_card: Some(TargetFilter::SelfRef),
            description: Some(text.to_string()),
            ..ReplacementDefinition::new(ReplacementEvent::Moved)
        });
    }

    // --- "If ~ would die, {effect}" ---
    if norm_lower.contains("~ would die") || norm_lower.contains("~ would be destroyed") {
        let effect_text = extract_replacement_effect(&normalized);
        let execute = effect_text.map(|e| Box::new(parse_effect_chain(&e, AbilityKind::Spell)));
        return Some(ReplacementDefinition {
            execute,
            valid_card: Some(TargetFilter::SelfRef),
            description: Some(text.to_string()),
            ..ReplacementDefinition::new(ReplacementEvent::Destroy)
        });
    }

    // --- "Prevent all combat damage" / "damage ... can't be prevented" ---
    if lower.contains("prevent all") && lower.contains("damage") {
        return Some(ReplacementDefinition {
            description: Some(text.to_string()),
            ..ReplacementDefinition::new(ReplacementEvent::DamageDone)
        });
    }
    if lower.contains("damage") && lower.contains("can't be prevented") {
        return Some(ReplacementDefinition {
            description: Some(text.to_string()),
            ..ReplacementDefinition::new(ReplacementEvent::DamageDone)
        });
    }

    // --- "If you would draw a card, {effect}" ---
    if lower.contains("you would draw") {
        let effect_text = extract_replacement_effect(&normalized);
        let execute = effect_text.map(|e| Box::new(parse_effect_chain(&e, AbilityKind::Spell)));
        return Some(ReplacementDefinition {
            execute,
            description: Some(text.to_string()),
            ..ReplacementDefinition::new(ReplacementEvent::Draw)
        });
    }

    // --- "If you would gain life, {effect}" ---
    if lower.contains("you would gain life") {
        let effect_text = extract_replacement_effect(&normalized);
        let execute = effect_text.map(|e| Box::new(parse_effect_chain(&e, AbilityKind::Spell)));
        return Some(ReplacementDefinition {
            execute,
            description: Some(text.to_string()),
            ..ReplacementDefinition::new(ReplacementEvent::GainLife)
        });
    }

    // --- "If [someone] would lose life, they lose twice that much life instead" ---
    if lower.contains("would lose life") {
        return Some(ReplacementDefinition {
            description: Some(text.to_string()),
            ..ReplacementDefinition::new(ReplacementEvent::LoseLife)
        });
    }

    None
}

/// Case-insensitive replacement of card name and self-referencing phrases with "~".
fn replace_self_refs(text: &str, card_name: &str) -> String {
    let result = text.replace(card_name, "~");
    // Case-insensitive replacement for self-referencing phrases
    ["this creature", "this land", "this permanent"]
        .iter()
        .fold(result, |acc, phrase| {
            case_insensitive_replace(&acc, phrase, "~")
        })
}

fn case_insensitive_replace(text: &str, pattern: &str, replacement: &str) -> String {
    let lower = text.to_lowercase();
    let pattern_lower = pattern.to_lowercase();
    match lower.find(&pattern_lower) {
        Some(pos) => {
            let mut result = String::with_capacity(text.len());
            result.push_str(&text[..pos]);
            result.push_str(replacement);
            result.push_str(&text[pos + pattern.len()..]);
            result
        }
        None => text.to_string(),
    }
}

/// Parse shock land pattern: "As ~ enters, you may pay N life. If you don't, it enters tapped."
/// Returns Optional ReplacementDefinition with execute=LoseLife (accept) and decline=Tap (decline).
fn parse_shock_land(norm_lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    // Match: "you may pay N life" + "enters tapped" (in either sentence order)
    if !norm_lower.contains("you may pay") || !norm_lower.contains("life") {
        return None;
    }
    if !norm_lower.contains("enters tapped")
        && !norm_lower.contains("enters the battlefield tapped")
    {
        return None;
    }

    // Extract life amount: "pay 2 life", "pay 3 life", etc.
    let amount = extract_life_payment(norm_lower)?;

    let execute = AbilityDefinition {
        kind: AbilityKind::Spell,
        effect: Effect::LoseLife { amount },
        cost: None,
        sub_ability: None,
        duration: None,
        description: None,
        target_prompt: None,
        sorcery_speed: false,
        condition: None,
    };

    let decline = AbilityDefinition {
        kind: AbilityKind::Spell,
        effect: Effect::Tap {
            target: TargetFilter::SelfRef,
        },
        cost: None,
        sub_ability: None,
        duration: None,
        description: None,
        target_prompt: None,
        sorcery_speed: false,
        condition: None,
    };

    Some(ReplacementDefinition {
        execute: Some(Box::new(execute)),
        mode: ReplacementMode::Optional {
            decline: Some(Box::new(decline)),
        },
        valid_card: Some(TargetFilter::SelfRef),
        description: Some(original_text.to_string()),
        ..ReplacementDefinition::new(ReplacementEvent::Moved)
    })
}

/// Parse check land pattern: "enters tapped unless you control a [LandType] or a [LandType]"
/// Returns Mandatory ReplacementDefinition with an UnlessControlsSubtype condition.
fn parse_check_land(norm_lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    if !norm_lower.contains("enters tapped")
        && !norm_lower.contains("enters the battlefield tapped")
    {
        return None;
    }

    let unless_idx = norm_lower.find("unless you control ")?;
    let rest = &norm_lower[unless_idx + "unless you control ".len()..];
    let rest = rest.trim_end_matches('.');

    let mut subtypes = Vec::new();
    for part in rest.split(" or ") {
        let trimmed = part
            .trim()
            .trim_start_matches("a ")
            .trim_start_matches("an ");
        let canonical = canonical_land_subtype(trimmed)?;
        if !subtypes.contains(&canonical) {
            subtypes.push(canonical);
        }
    }

    if subtypes.is_empty() {
        return None;
    }

    let tap_effect = Box::new(AbilityDefinition {
        kind: AbilityKind::Spell,
        effect: Effect::Tap {
            target: TargetFilter::SelfRef,
        },
        cost: None,
        sub_ability: None,
        duration: None,
        description: None,
        target_prompt: None,
        sorcery_speed: false,
        condition: None,
    });

    Some(ReplacementDefinition {
        execute: Some(tap_effect),
        valid_card: Some(TargetFilter::SelfRef),
        description: Some(original_text.to_string()),
        condition: Some(ReplacementCondition::UnlessControlsSubtype { subtypes }),
        ..ReplacementDefinition::new(ReplacementEvent::Moved)
    })
}

/// Map lowercase land subtype name to canonical (title-cased) form.
fn canonical_land_subtype(raw: &str) -> Option<String> {
    match raw {
        "plains" => Some("Plains".to_string()),
        "island" => Some("Island".to_string()),
        "swamp" => Some("Swamp".to_string()),
        "mountain" => Some("Mountain".to_string()),
        "forest" => Some("Forest".to_string()),
        _ => None,
    }
}

/// Extract life payment amount from "pay N life" pattern.
fn extract_life_payment(text: &str) -> Option<i32> {
    let pay_idx = text.find("pay ")?;
    let after_pay = &text[pay_idx + 4..];
    let end = after_pay.find(' ').unwrap_or(after_pay.len());
    let num_str = &after_pay[..end];
    num_str.parse().ok()
}

fn extract_replacement_effect(text: &str) -> Option<String> {
    // Find ", " after "would" or "instead" clause
    if let Some(pos) = text.find(", ") {
        let effect = text[pos + 2..].trim();
        if !effect.is_empty() {
            return Some(effect.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_enters_tapped() {
        let def =
            parse_replacement_line("Gutterbones enters the battlefield tapped.", "Gutterbones")
                .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
    }

    #[test]
    fn replacement_prevent_all_combat_damage() {
        let def = parse_replacement_line(
            "Prevent all combat damage that would be dealt to you.",
            "Some Card",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
    }

    #[test]
    fn replacement_damage_cant_be_prevented() {
        let def = parse_replacement_line(
            "Combat damage that would be dealt by creatures you control can't be prevented.",
            "Questing Beast",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
    }

    #[test]
    fn replacement_lose_life_doubled() {
        let def = parse_replacement_line(
            "If an opponent would lose life during your turn, they lose twice that much life instead.",
            "Bloodletter of Aclazotz",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::LoseLife);
        assert!(def.description.is_some());
    }

    #[test]
    fn replacement_non_match_returns_none() {
        assert!(parse_replacement_line("Destroy target creature.", "Some Card").is_none());
    }

    #[test]
    fn shock_land_watery_grave() {
        let def = parse_replacement_line(
            "As this land enters, you may pay 2 life. If you don't, it enters tapped.",
            "Watery Grave",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Optional { .. }));
        // Accept branch: LoseLife { amount: 2 }
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(execute.effect, Effect::LoseLife { amount: 2 }));
        // Decline branch: Tap { target: SelfRef }
        if let ReplacementMode::Optional { decline } = &def.mode {
            let decline = decline.as_ref().unwrap();
            assert!(matches!(
                decline.effect,
                Effect::Tap {
                    target: TargetFilter::SelfRef
                }
            ));
        } else {
            panic!("Expected Optional mode");
        }
    }

    #[test]
    fn shock_land_3_life() {
        let def = parse_replacement_line(
            "As this land enters, you may pay 3 life. If you don't, it enters tapped.",
            "Some Shock Land",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(execute.effect, Effect::LoseLife { amount: 3 }));
    }

    #[test]
    fn check_land_clifftop_retreat() {
        let def = parse_replacement_line(
            "This land enters tapped unless you control a Mountain or a Plains.",
            "Clifftop Retreat",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        assert!(matches!(
            def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsSubtype { subtypes }) => {
                assert_eq!(subtypes, &["Mountain", "Plains"]);
            }
            other => panic!("Expected UnlessControlsSubtype, got {other:?}"),
        }
    }

    #[test]
    fn check_land_drowned_catacomb() {
        let def = parse_replacement_line(
            "Drowned Catacomb enters the battlefield tapped unless you control an Island or a Swamp.",
            "Drowned Catacomb",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsSubtype { subtypes }) => {
                assert_eq!(subtypes, &["Island", "Swamp"]);
            }
            other => panic!("Expected UnlessControlsSubtype, got {other:?}"),
        }
    }

    #[test]
    fn unconditional_enters_tapped_still_works() {
        let def = parse_replacement_line(
            "Submerged Boneyard enters the battlefield tapped.",
            "Submerged Boneyard",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        // execute must be Some(Tap) so the mandatory pipeline can apply it
        assert!(matches!(
            def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
    }
}
