use crate::types::ability::{AbilityDefinition, AbilityKind, Effect};

use super::oracle::has_unimplemented;
use super::oracle_classifier::{
    has_trigger_prefix, is_damage_prevention_pattern, is_effect_sentence_candidate,
    is_replacement_pattern, is_static_pattern,
};
use super::oracle_effect::{parse_effect_chain_with_context, ParseContext};

pub(super) fn dispatch_line_nom(line: &str, card_name: &str) -> Effect {
    let lower = line.to_lowercase();
    let ctx = ParseContext {
        subject: None,
        card_name: Some(card_name.to_string()),
        actor: None,
        ..Default::default()
    };

    if is_effect_sentence_candidate(&lower) || is_damage_prevention_pattern(&lower) {
        let def = parse_effect_chain_with_context(line, AbilityKind::Spell, &ctx);
        if !has_unimplemented(&def) {
            return *def.effect;
        }
    }

    let lower_trimmed = lower.trim_start();
    if has_trigger_prefix(lower_trimmed) {
        return Effect::Unimplemented {
            name: "trigger_structure".into(),
            description: Some(format!(
                "Trigger prefix matched but line failed trigger parser: {line}"
            )),
        };
    }

    if is_static_pattern(&lower) {
        return Effect::Unimplemented {
            name: "static_structure".into(),
            description: Some(format!(
                "Static pattern matched but line failed static parser: {line}"
            )),
        };
    }

    if is_replacement_pattern(&lower) {
        return Effect::Unimplemented {
            name: "replacement_structure".into(),
            description: Some(format!(
                "Replacement pattern matched but line failed replacement parser: {line}"
            )),
        };
    }

    if is_effect_sentence_candidate(&lower) {
        return Effect::Unimplemented {
            name: "effect_structure".into(),
            description: Some(format!(
                "Effect sentence candidate but line failed effect parser: {line}"
            )),
        };
    }

    Effect::Unimplemented {
        name: "unknown".into(),
        description: Some(line.to_string()),
    }
}

pub(super) fn make_unimplemented_with_effect(line: &str, effect: Effect) -> AbilityDefinition {
    if !matches!(effect, Effect::Unimplemented { .. }) {
        return AbilityDefinition::new(AbilityKind::Spell, effect).description(line.to_string());
    }

    tracing::warn!(oracle_text = line, "unimplemented ability line");
    AbilityDefinition::new(AbilityKind::Spell, effect).description(line.to_string())
}
