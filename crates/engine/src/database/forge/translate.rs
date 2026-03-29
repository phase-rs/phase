use tracing::trace;

use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ModalChoice, ReplacementDefinition, StaticDefinition,
    TargetFilter, TriggerDefinition,
};
use crate::types::card::CardFace;
use crate::types::keywords::Keyword;

use super::cost::translate_cost;
use super::effect::translate_effect;
use super::filter::translate_filter;
use super::keyword::translate_keyword;
use super::loader::ForgeIndex;
use super::replacement::translate_replacement;
use super::static_ab::translate_static;
use super::svar::SvarResolver;
use super::trigger::translate_trigger;
use super::types::ForgeCard;

/// Result of translating a Forge card into phase.rs types.
struct TranslatedCard {
    abilities: Vec<AbilityDefinition>,
    triggers: Vec<TriggerDefinition>,
    statics: Vec<StaticDefinition>,
    replacements: Vec<ReplacementDefinition>,
    keywords: Vec<Keyword>,
}

/// Translate a `ForgeCard` AST into phase.rs types.
fn translate_card(forge_card: &ForgeCard) -> TranslatedCard {
    let mut resolver = SvarResolver::new(&forge_card.svars);

    // Translate abilities (A: lines)
    let abilities: Vec<AbilityDefinition> = forge_card
        .abilities
        .iter()
        .filter_map(|line| {
            let params = &line.params;

            // Check for Charm → modal handling
            if params.effect_type() == Some("Charm") {
                return translate_charm(params, &mut resolver, line.raw.as_str());
            }

            match translate_effect(params, &mut resolver) {
                Ok(effect) => {
                    let mut ability = AbilityDefinition::new(AbilityKind::Spell, effect);

                    // Wire cost if present
                    if let Some(cost_str) = params.get("Cost") {
                        if let Ok(cost) = translate_cost(cost_str) {
                            ability.cost = Some(cost);
                            ability.kind = AbilityKind::Activated;
                        }
                    }

                    // Wire target filter
                    if let Some(tgt_str) = params.get("ValidTgts") {
                        if let Ok(filter) = translate_filter(tgt_str) {
                            if filter != TargetFilter::Any {
                                // Target prompt
                                if let Some(prompt) = params.get("TgtPrompt") {
                                    ability.target_prompt = Some(prompt.to_string());
                                }
                            }
                        }
                    }

                    // Wire SubAbility$ chain
                    if let Some(sub_name) = params.get("SubAbility") {
                        if let Ok(sub) = resolver.resolve_ability(sub_name) {
                            ability.sub_ability = Some(Box::new(sub));
                        }
                    }

                    // Wire description
                    if let Some(desc) = params.get("SpellDescription") {
                        ability.description = Some(desc.to_string());
                    }

                    Some(ability)
                }
                Err(_) => {
                    // Graceful degradation — return Unimplemented with forge: prefix
                    let effect_type = params.effect_type().unwrap_or("unknown");
                    Some(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Unimplemented {
                            name: format!("forge:{effect_type}"),
                            description: Some(line.raw.clone()),
                        },
                    ))
                }
            }
        })
        .collect();

    // Translate triggers (T: lines)
    let triggers: Vec<TriggerDefinition> = forge_card
        .triggers
        .iter()
        .filter_map(|line| translate_trigger(line, &mut resolver).ok())
        .collect();

    // Translate statics (S: lines)
    let statics: Vec<StaticDefinition> = forge_card
        .statics
        .iter()
        .filter_map(|line| translate_static(line).ok())
        .collect();

    // Translate replacements (R: lines)
    let replacements: Vec<ReplacementDefinition> = forge_card
        .replacements
        .iter()
        .filter_map(|line| translate_replacement(line, &mut resolver).ok())
        .collect();

    // Translate keywords (K: lines)
    let keywords: Vec<Keyword> = forge_card
        .keywords
        .iter()
        .filter_map(|kw_line| translate_keyword(kw_line))
        .collect();

    TranslatedCard {
        abilities,
        triggers,
        statics,
        replacements,
        keywords,
    }
}

/// Translate a Forge Charm (modal spell) via SVar resolution.
fn translate_charm(
    params: &super::types::ForgeParams,
    resolver: &mut SvarResolver,
    _raw: &str,
) -> Option<AbilityDefinition> {
    let choices_str = params.get("Choices")?;
    let choice_names: Vec<&str> = choices_str.split(',').map(|s| s.trim()).collect();

    let mut mode_abilities = Vec::new();
    let mut mode_descriptions = Vec::new();

    for name in &choice_names {
        match resolver.resolve_ability(name) {
            Ok(ability) => {
                let desc = ability
                    .description
                    .clone()
                    .unwrap_or_else(|| name.to_string());
                mode_descriptions.push(desc);
                mode_abilities.push(ability);
            }
            Err(_) => {
                mode_descriptions.push(name.to_string());
                mode_abilities.push(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Unimplemented {
                        name: format!("forge:Charm:{name}"),
                        description: None,
                    },
                ));
            }
        }
    }

    let modal = ModalChoice {
        min_choices: 1,
        max_choices: 1,
        mode_count: mode_abilities.len(),
        mode_descriptions,
        allow_repeat_modes: false,
        constraints: Vec::new(),
        mode_costs: Vec::new(),
    };

    let mut ability = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Unimplemented {
            name: "modal_placeholder".to_string(),
            description: None,
        },
    );
    ability.modal = Some(modal);
    ability.mode_abilities = mode_abilities;

    if let Some(desc) = params.get("SpellDescription") {
        ability.description = Some(desc.to_string());
    }

    Some(ability)
}

/// Apply Forge data as a fallback for `Unimplemented` entries in an Oracle-parsed face.
///
/// Only replaces entries that Oracle couldn't parse. Working Oracle entries are
/// never touched. This is the main integration point.
pub fn apply_forge_fallback(face: &mut CardFace, forge_index: &ForgeIndex) {
    let name_lower = face.name.to_lowercase();
    let forge_card = match forge_index.parse_card(&name_lower) {
        Some(c) => c,
        None => return,
    };

    let translated = translate_card(&forge_card);

    // Per-ability granular fallback: only replace Unimplemented entries.
    replace_unimplemented_abilities(face, &translated.abilities);
    replace_unimplemented_triggers(face, &translated.triggers);
    replace_unimplemented_statics(face, &translated.statics);
    replace_unimplemented_replacements(face, &translated.replacements);

    // Keywords: append any Forge keywords not already present in Oracle-parsed set.
    for kw in &translated.keywords {
        if !face.keywords.contains(kw) {
            face.keywords.push(kw.clone());
        }
    }

    trace!(
        card = face.name,
        forge_abilities = translated.abilities.len(),
        forge_triggers = translated.triggers.len(),
        forge_statics = translated.statics.len(),
        forge_replacements = translated.replacements.len(),
        "applied forge fallback"
    );
}

/// Replace Unimplemented ability entries with Forge translations.
///
/// Strategy: Walk Oracle abilities. For each Unimplemented entry, count its
/// position among Unimplemented entries. Match that Nth-unimplemented to the
/// Nth Forge ability.
fn replace_unimplemented_abilities(face: &mut CardFace, forge_abilities: &[AbilityDefinition]) {
    if forge_abilities.is_empty() {
        return;
    }

    let mut unimpl_idx = 0;
    for ability in &mut face.abilities {
        if matches!(&*ability.effect, Effect::Unimplemented { .. }) {
            if let Some(forge_ability) = forge_abilities.get(unimpl_idx) {
                // Only replace if the Forge version isn't also Unimplemented
                if !matches!(&*forge_ability.effect, Effect::Unimplemented { .. }) {
                    *ability = forge_ability.clone();
                }
            }
            unimpl_idx += 1;
        }
    }

    // Append: if Forge has more abilities than Oracle parsed in total
    if forge_abilities.len() > face.abilities.len() {
        for forge_ability in &forge_abilities[face.abilities.len()..] {
            if !matches!(&*forge_ability.effect, Effect::Unimplemented { .. }) {
                face.abilities.push(forge_ability.clone());
            }
        }
    }
}

/// Replace unimplemented triggers by matching on TriggerMode (semantic anchor).
fn replace_unimplemented_triggers(face: &mut CardFace, forge_triggers: &[TriggerDefinition]) {
    if forge_triggers.is_empty() {
        return;
    }

    // For each Oracle trigger that has an Unimplemented execute, try to find
    // a matching Forge trigger by mode.
    for trigger in &mut face.triggers {
        let has_unimpl_execute = trigger
            .execute
            .as_ref()
            .is_none_or(|exec| matches!(&*exec.effect, Effect::Unimplemented { .. }));

        if has_unimpl_execute {
            // Find matching Forge trigger by mode
            if let Some(forge_trigger) = forge_triggers.iter().find(|ft| ft.mode == trigger.mode) {
                if let Some(ref exec) = forge_trigger.execute {
                    trigger.execute = Some(exec.clone());
                }
            }
        }
    }

    // Append triggers that Oracle didn't parse at all
    if forge_triggers.len() > face.triggers.len() {
        let oracle_modes: Vec<_> = face.triggers.iter().map(|t| t.mode.clone()).collect();
        for forge_trigger in forge_triggers {
            if !oracle_modes.contains(&forge_trigger.mode) {
                face.triggers.push(forge_trigger.clone());
            }
        }
    }
}

/// Replace unimplemented statics by matching on StaticMode.
///
/// Strategy: For each Oracle static with empty modifications (likely a placeholder),
/// find a Forge static with the same mode and replace it. Then append any Forge
/// statics whose modes don't appear in the Oracle set at all.
fn replace_unimplemented_statics(face: &mut CardFace, forge_statics: &[StaticDefinition]) {
    if forge_statics.is_empty() {
        return;
    }

    // Replace existing placeholder statics by mode
    for oracle_static in &mut face.static_abilities {
        let is_placeholder =
            oracle_static.modifications.is_empty() && oracle_static.affected.is_none();
        if is_placeholder {
            if let Some(forge_static) = forge_statics
                .iter()
                .find(|fs| fs.mode == oracle_static.mode)
            {
                *oracle_static = forge_static.clone();
            }
        }
    }

    // Append statics whose modes don't exist in Oracle's set
    let oracle_modes: Vec<_> = face
        .static_abilities
        .iter()
        .map(|s| s.mode.clone())
        .collect();
    for forge_static in forge_statics {
        if !oracle_modes.contains(&forge_static.mode) {
            face.static_abilities.push(forge_static.clone());
        }
    }
}

/// Replace unimplemented replacements by matching on ReplacementEvent.
///
/// Strategy: For each Oracle replacement with no execute chain (placeholder),
/// find a Forge replacement with the same event and replace it. Then append any
/// Forge replacements whose events don't appear in the Oracle set at all.
fn replace_unimplemented_replacements(
    face: &mut CardFace,
    forge_replacements: &[ReplacementDefinition],
) {
    if forge_replacements.is_empty() {
        return;
    }

    // Replace existing placeholder replacements by event
    for oracle_repl in &mut face.replacements {
        let is_placeholder = oracle_repl.execute.is_none();
        if is_placeholder {
            if let Some(forge_repl) = forge_replacements
                .iter()
                .find(|fr| fr.event == oracle_repl.event)
            {
                *oracle_repl = forge_repl.clone();
            }
        }
    }

    // Append replacements whose events don't exist in Oracle's set
    let oracle_events: Vec<_> = face.replacements.iter().map(|r| r.event.clone()).collect();
    for forge_repl in forge_replacements {
        if !oracle_events.contains(&forge_repl.event) {
            face.replacements.push(forge_repl.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::database::forge::loader::parse_params;
    use crate::database::forge::types::{ForgeAbilityLine, ForgeCard};

    fn make_forge_card(abilities: &[&str], svars: &[(&str, &str)]) -> ForgeCard {
        ForgeCard {
            name: "Test Card".to_string(),
            mana_cost: None,
            types: None,
            pt: None,
            colors: None,
            abilities: abilities
                .iter()
                .map(|a| ForgeAbilityLine {
                    raw: a.to_string(),
                    params: parse_params(a),
                })
                .collect(),
            triggers: Vec::new(),
            statics: Vec::new(),
            replacements: Vec::new(),
            keywords: Vec::new(),
            svars: svars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            oracle_text: None,
            alternate: None,
        }
    }

    #[test]
    fn test_translate_lightning_bolt() {
        let card = make_forge_card(
            &["SP$ DealDamage | ValidTgts$ Any | NumDmg$ 3 | SpellDescription$ CARDNAME deals 3 damage to any target."],
            &[],
        );
        let translated = translate_card(&card);

        assert_eq!(translated.abilities.len(), 1);
        match &*translated.abilities[0].effect {
            Effect::DealDamage { amount, target, .. } => {
                assert_eq!(
                    *amount,
                    crate::types::ability::QuantityExpr::Fixed { value: 3 }
                );
                assert_eq!(*target, TargetFilter::Any);
            }
            other => panic!("expected DealDamage, got {other:?}"),
        }
    }

    #[test]
    fn test_apply_fallback_only_replaces_unimplemented() {
        // Create a CardFace with 2 abilities: one working, one Unimplemented
        let mut face = CardFace {
            name: "Test Card".to_string(),
            ..CardFace::default()
        };

        let working = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        let unimpl = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Unimplemented {
                name: "some_effect".to_string(),
                description: None,
            },
        );
        face.abilities = vec![working.clone(), unimpl];

        // Forge has a replacement for the Unimplemented
        let forge_replacement = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            },
        );

        replace_unimplemented_abilities(&mut face, &[forge_replacement]);

        // First ability (working) should be untouched
        assert!(matches!(
            &*face.abilities[0].effect,
            Effect::DealDamage { .. }
        ));
        // Second ability (was Unimplemented) should now be Draw
        assert!(matches!(&*face.abilities[1].effect, Effect::Draw { .. }));
    }
}
