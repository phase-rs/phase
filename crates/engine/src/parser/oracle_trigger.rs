use crate::types::ability::{AbilityKind, TargetFilter, TriggerDefinition};
use crate::types::triggers::TriggerMode;
use crate::types::phase::Phase;
use crate::types::zones::Zone;
use super::oracle_effect::parse_effect_chain;
use super::oracle_util::strip_reminder_text;

/// Parse a full trigger line into a TriggerDefinition.
/// Input: a line starting with "When", "Whenever", or "At".
/// The card_name is used for self-reference substitution.
pub fn parse_trigger_line(text: &str, card_name: &str) -> TriggerDefinition {
    let text = strip_reminder_text(text);
    // Replace self-references: "this creature", "this enchantment", card name → ~
    let normalized = normalize_self_refs(&text, card_name);
    let lower = normalized.to_lowercase();

    // Split condition from effect at first ", " after the trigger phrase
    let (condition_text, effect_text) = split_trigger(&lower, &normalized);

    // Check for "you may" optional
    let optional = effect_text.to_lowercase().contains("you may ");
    let effect_clean = effect_text
        .to_lowercase()
        .replace("you may ", "")
        .trim()
        .to_string();

    // Parse the effect
    let execute = if !effect_clean.is_empty() {
        Some(Box::new(parse_effect_chain(&effect_clean, AbilityKind::Spell)))
    } else {
        None
    };

    // Parse the condition
    let (_, mut def) = parse_trigger_condition(&condition_text);
    def.execute = execute;
    def.optional = optional;
    def
}

fn normalize_self_refs(text: &str, card_name: &str) -> String {
    text.replace(card_name, "~")
        .replace("this creature", "~")
        .replace("this enchantment", "~")
        .replace("this artifact", "~")
        .replace("This creature", "~")
        .replace("This enchantment", "~")
        .replace("This artifact", "~")
}

fn split_trigger<'a>(lower: &str, original: &'a str) -> (String, String) {
    if let Some(comma_pos) = find_effect_boundary(lower) {
        let condition = original[..comma_pos].trim().to_string();
        let effect = original[comma_pos + 2..].trim().to_string();
        (condition, effect)
    } else {
        (original.to_string(), String::new())
    }
}

fn find_effect_boundary(lower: &str) -> Option<usize> {
    lower.find(", ")
}

fn make_base() -> TriggerDefinition {
    TriggerDefinition {
        mode: TriggerMode::Unknown("unknown".to_string()),
        execute: None,
        valid_card: None,
        origin: None,
        destination: None,
        trigger_zones: vec![Zone::Battlefield],
        phase: None,
        optional: false,
        combat_damage: false,
        secondary: false,
        valid_target: None,
        valid_source: None,
        description: None,
    }
}

fn parse_trigger_condition(condition: &str) -> (TriggerMode, TriggerDefinition) {
    let lower = condition.to_lowercase();

    // --- "When ~ enters [the battlefield]" ---
    if lower.contains("~ enters") {
        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.destination = Some(Zone::Battlefield);
        def.valid_card = Some(TargetFilter::SelfRef);
        return (TriggerMode::ChangesZone, def);
    }

    // --- "When ~ dies" ---
    if lower.contains("~ dies") {
        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.origin = Some(Zone::Battlefield);
        def.destination = Some(Zone::Graveyard);
        def.valid_card = Some(TargetFilter::SelfRef);
        return (TriggerMode::ChangesZone, def);
    }

    // --- "Whenever ~ deals combat damage to a player/opponent" ---
    if lower.contains("~ deals combat damage") {
        let mut def = make_base();
        def.mode = TriggerMode::DamageDone;
        def.combat_damage = true;
        def.valid_source = Some(TargetFilter::SelfRef);
        return (TriggerMode::DamageDone, def);
    }

    // --- "Whenever ~ deals damage" ---
    if lower.contains("~ deals damage") {
        let mut def = make_base();
        def.mode = TriggerMode::DamageDone;
        def.valid_source = Some(TargetFilter::SelfRef);
        return (TriggerMode::DamageDone, def);
    }

    // --- "Whenever ~ attacks" ---
    if lower.contains("~ attacks") {
        let mut def = make_base();
        def.mode = TriggerMode::Attacks;
        def.valid_card = Some(TargetFilter::SelfRef);
        return (TriggerMode::Attacks, def);
    }

    // --- "Whenever a creature enters" ---
    if lower.contains("a creature enters") || lower.contains("a creature you control enters") {
        let mut def = make_base();
        def.mode = TriggerMode::ChangesZone;
        def.destination = Some(Zone::Battlefield);
        return (TriggerMode::ChangesZone, def);
    }

    // --- "Whenever you cast a spell" ---
    if lower.contains("you cast a") || lower.contains("you cast an") {
        let mut def = make_base();
        def.mode = TriggerMode::SpellCast;
        return (TriggerMode::SpellCast, def);
    }

    // --- "Whenever you gain life" ---
    if lower.contains("you gain life") {
        let mut def = make_base();
        def.mode = TriggerMode::LifeGained;
        return (TriggerMode::LifeGained, def);
    }

    // --- "Whenever you draw a card" ---
    if lower.contains("you draw a card") {
        let mut def = make_base();
        def.mode = TriggerMode::Drawn;
        return (TriggerMode::Drawn, def);
    }

    // --- Phase triggers: "At the beginning of..." ---
    if lower.starts_with("at the beginning of") {
        let phase_text = lower[19..].trim();
        let mut def = make_base();
        def.mode = TriggerMode::Phase;
        if phase_text.contains("upkeep") {
            def.phase = Some(Phase::Upkeep);
        } else if phase_text.contains("end step") {
            def.phase = Some(Phase::End);
        } else if phase_text.contains("combat") {
            def.phase = Some(Phase::BeginCombat);
        } else if phase_text.contains("draw step") {
            def.phase = Some(Phase::Draw);
        }
        return (TriggerMode::Phase, def);
    }

    // --- Fallback ---
    let mut def = make_base();
    let mode = TriggerMode::Unknown(condition.to_string());
    def.mode = mode.clone();
    def.description = Some(condition.to_string());
    (mode, def)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(def.combat_damage);
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
}
