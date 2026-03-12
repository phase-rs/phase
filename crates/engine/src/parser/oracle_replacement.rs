use super::oracle_effect::parse_effect_chain;
use super::oracle_util::strip_reminder_text;
use crate::types::ability::{AbilityKind, ReplacementDefinition, TargetFilter};
use crate::types::replacements::ReplacementEvent;

/// Parse a replacement effect line into a ReplacementDefinition.
/// Handles: "If ~ would die", "Prevent all combat damage",
/// "~ enters the battlefield tapped", etc.
pub fn parse_replacement_line(text: &str, card_name: &str) -> Option<ReplacementDefinition> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();
    let normalized = text
        .replace(card_name, "~")
        .replace("this creature", "~")
        .replace("This creature", "~");
    let norm_lower = normalized.to_lowercase();

    // --- "~ enters the battlefield tapped" ---
    if norm_lower.contains("enters the battlefield tapped") || norm_lower.contains("enters tapped")
    {
        return Some(ReplacementDefinition {
            event: ReplacementEvent::Moved,
            execute: None,
            valid_card: Some(TargetFilter::SelfRef),
            description: Some(text.to_string()),
        });
    }

    // --- "If ~ would die, {effect}" ---
    if norm_lower.contains("~ would die") || norm_lower.contains("~ would be destroyed") {
        let effect_text = extract_replacement_effect(&normalized);
        let execute = effect_text.map(|e| Box::new(parse_effect_chain(&e, AbilityKind::Spell)));
        return Some(ReplacementDefinition {
            event: ReplacementEvent::Destroy,
            execute,
            valid_card: Some(TargetFilter::SelfRef),
            description: Some(text.to_string()),
        });
    }

    // --- "Prevent all combat damage" / "damage ... can't be prevented" ---
    if lower.contains("prevent all") && lower.contains("damage") {
        return Some(ReplacementDefinition {
            event: ReplacementEvent::DamageDone,
            execute: None,
            valid_card: None,
            description: Some(text.to_string()),
        });
    }
    if lower.contains("damage") && lower.contains("can't be prevented") {
        return Some(ReplacementDefinition {
            event: ReplacementEvent::DamageDone,
            execute: None,
            valid_card: None,
            description: Some(text.to_string()),
        });
    }

    // --- "If you would draw a card, {effect}" ---
    if lower.contains("you would draw") {
        let effect_text = extract_replacement_effect(&normalized);
        let execute = effect_text.map(|e| Box::new(parse_effect_chain(&e, AbilityKind::Spell)));
        return Some(ReplacementDefinition {
            event: ReplacementEvent::Draw,
            execute,
            valid_card: None,
            description: Some(text.to_string()),
        });
    }

    // --- "If you would gain life, {effect}" ---
    if lower.contains("you would gain life") {
        let effect_text = extract_replacement_effect(&normalized);
        let execute = effect_text.map(|e| Box::new(parse_effect_chain(&e, AbilityKind::Spell)));
        return Some(ReplacementDefinition {
            event: ReplacementEvent::GainLife,
            execute,
            valid_card: None,
            description: Some(text.to_string()),
        });
    }

    // --- "If [someone] would lose life, they lose twice that much life instead" ---
    if lower.contains("would lose life") {
        return Some(ReplacementDefinition {
            event: ReplacementEvent::LoseLife,
            execute: None,
            valid_card: None,
            description: Some(text.to_string()),
        });
    }

    None
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
}
