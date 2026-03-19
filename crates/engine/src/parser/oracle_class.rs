use crate::types::ability::{
    AbilityDefinition, AbilityKind, ActivationRestriction, Effect, StaticCondition,
    StaticDefinition, TargetFilter, TriggerCondition, TriggerConstraint, TriggerDefinition,
};
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::oracle::{
    has_unimplemented, is_effect_sentence_candidate, is_granted_static_line,
    is_replacement_pattern, is_static_pattern, make_unimplemented, normalize_self_refs_for_static,
    ParsedAbilities,
};
use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::parse_effect_chain;
use super::oracle_keyword::extract_keyword_line;
use super::oracle_modal::strip_ability_word;
use super::oracle_replacement::parse_replacement_line;
use super::oracle_static::parse_static_line;
use super::oracle_trigger::parse_trigger_line;
use super::oracle_util::strip_reminder_text;

/// Detect a "{cost}: Level N" line using structural parsing.
/// Returns `(level_number, cost_text)` if the line matches.
pub(crate) fn parse_class_level_line(line: &str) -> Option<(u8, String)> {
    let colon_pos = super::oracle::find_activated_colon(line)?;
    let cost_text = line[..colon_pos].trim();
    let effect_text = line[colon_pos + 1..].trim();
    let lower_effect = effect_text.to_lowercase();

    // Check if the effect portion is "Level N"
    let rest = lower_effect.strip_prefix("level ")?;
    let (n, remainder) = super::oracle_util::parse_number(rest)?;
    // Must be exactly "Level N" with nothing else
    if !remainder.trim().is_empty() {
        return None;
    }
    Some((n as u8, cost_text.to_string()))
}

/// CR 716: Parse Class enchantment Oracle text into level-gated abilities.
///
/// Splits the Oracle text into level sections by detecting "{cost}: Level N" lines,
/// then parses each section's ability lines through existing machinery and wraps
/// them with level-gating conditions (StaticCondition::ClassLevelGE for statics,
/// TriggerCondition::ClassLevelGE for continuous triggers, TriggerConstraint::AtClassLevel
/// for "When this Class becomes level N" triggers).
pub(crate) fn parse_class_oracle_text(
    lines: &[&str],
    card_name: &str,
    mtgjson_keyword_names: &[String],
    mut result: ParsedAbilities,
) -> ParsedAbilities {
    // Split lines into level sections: (level, lines)
    // Level 1 section has level=1, subsequent sections have level=2, 3, etc.
    struct LevelSection {
        level: u8,
        /// For levels > 1: cost text and the level line description.
        level_up: Option<(String, String)>,
        lines: Vec<String>,
    }

    let mut sections: Vec<LevelSection> = vec![LevelSection {
        level: 1,
        level_up: None,
        lines: Vec::new(),
    }];

    for &raw_line in lines {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let stripped = strip_reminder_text(trimmed);
        if stripped.is_empty() {
            continue;
        }

        if let Some((level, cost_text)) = parse_class_level_line(&stripped) {
            sections.push(LevelSection {
                level,
                level_up: Some((cost_text, stripped.to_string())),
                lines: Vec::new(),
            });
        } else {
            // Add line to the current (last) section
            if let Some(section) = sections.last_mut() {
                section.lines.push(stripped);
            }
        }
    }

    // Process each level section
    for section in &sections {
        // Generate the "{cost}: Level N" activated ability
        if let Some((cost_text, description)) = &section.level_up {
            let cost = parse_oracle_cost(cost_text);
            let mut def = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::SetClassLevel {
                    level: section.level,
                },
            );
            def.cost = Some(cost);
            def.description = Some(description.clone());
            def.sorcery_speed = true;
            // CR 716.4: Level N+1 can only activate when at level N.
            def.activation_restrictions
                .push(ActivationRestriction::AsSorcery);
            def.activation_restrictions
                .push(ActivationRestriction::ClassLevelIs {
                    level: section.level - 1,
                });
            result.abilities.push(def);
        }

        // Parse ability lines for this level section
        for line in &section.lines {
            let lower = line.to_lowercase();
            let static_line = normalize_self_refs_for_static(line, card_name);

            // Check for "When this Class becomes level N" trigger pattern
            if is_class_level_trigger(&lower, card_name) {
                if let Some(trigger) = parse_class_level_trigger(line, card_name, section.level) {
                    result.triggers.push(trigger);
                    continue;
                }
            }

            // Keyword-only lines
            if let Some(extracted) = extract_keyword_line(line, mtgjson_keyword_names) {
                result.extracted_keywords.extend(extracted);
                continue;
            }

            // Triggered abilities (When/Whenever/At)
            if lower.starts_with("when ")
                || lower.starts_with("whenever ")
                || lower.starts_with("at ")
            {
                let mut trigger = parse_trigger_line(line, card_name);
                // CR 716.6: Gate continuous triggers at levels > 1.
                if section.level > 1 {
                    trigger.condition = Some(TriggerCondition::ClassLevelGE {
                        level: section.level,
                    });
                }
                result.triggers.push(trigger);
                continue;
            }

            // "Enchanted"/"Equipped"/"Creatures"/"All" granted statics (high priority)
            if is_granted_static_line(&lower) {
                if let Some(mut static_def) = parse_static_line(&static_line) {
                    if section.level > 1 {
                        static_def = wrap_static_with_class_level(static_def, section.level);
                    }
                    result.statics.push(static_def);
                    continue;
                }
            }

            // Static/continuous patterns
            if is_static_pattern(&lower) {
                if let Some(mut static_def) = parse_static_line(&static_line) {
                    if section.level > 1 {
                        static_def = wrap_static_with_class_level(static_def, section.level);
                    }
                    result.statics.push(static_def);
                    continue;
                }
            }

            // Replacement patterns
            if is_replacement_pattern(&lower) {
                if let Some(rep_def) = parse_replacement_line(line, card_name) {
                    // Note: replacement definitions don't have a condition field;
                    // they fire at all levels once added. This matches CR 716.6.
                    result.replacements.push(rep_def);
                    continue;
                }
            }

            // Ability word prefixed lines
            if let Some(effect_text) = strip_ability_word(line) {
                let effect_lower = effect_text.to_lowercase();
                if effect_lower.starts_with("when ")
                    || effect_lower.starts_with("whenever ")
                    || effect_lower.starts_with("at ")
                {
                    let mut trigger = parse_trigger_line(&effect_text, card_name);
                    if section.level > 1 {
                        trigger.condition = Some(TriggerCondition::ClassLevelGE {
                            level: section.level,
                        });
                    }
                    result.triggers.push(trigger);
                    continue;
                }
                if is_static_pattern(&effect_lower) {
                    let effect_static = normalize_self_refs_for_static(&effect_text, card_name);
                    if let Some(mut static_def) = parse_static_line(&effect_static) {
                        if section.level > 1 {
                            static_def = wrap_static_with_class_level(static_def, section.level);
                        }
                        result.statics.push(static_def);
                        continue;
                    }
                }
            }

            // Effect/spell-like lines (e.g., "You may play an additional land...")
            if is_effect_sentence_candidate(&lower) {
                let def = parse_effect_chain(line, AbilityKind::Spell);
                if !has_unimplemented(&def) {
                    result.abilities.push(def);
                    continue;
                }
            }

            // Fallback: unimplemented
            result.abilities.push(make_unimplemented(line));
        }
    }

    result
}

/// Check if a line matches "when this class becomes level N" pattern.
pub(crate) fn is_class_level_trigger(lower: &str, card_name: &str) -> bool {
    let card_lower = card_name.to_lowercase();
    // "When this Class becomes level N" or "When CARDNAME becomes level N"
    lower.starts_with("when ")
        && lower.contains("becomes level ")
        && (lower.contains("this class") || lower.contains(&card_lower))
}

/// Parse a "When this Class becomes level N, {effect}" trigger.
fn parse_class_level_trigger(line: &str, card_name: &str, level: u8) -> Option<TriggerDefinition> {
    // Find "becomes level N" and extract the effect after the comma
    let lower = line.to_lowercase();
    let becomes_pos = lower.find("becomes level ")?;
    let after_becomes = &line[becomes_pos + "becomes level ".len()..];

    // Parse the level number
    let after_lower = after_becomes.to_lowercase();
    let (_, rest) = super::oracle_util::parse_number(&after_lower)?;

    // The effect follows after ", " or just the rest of the text
    let effect_text = rest.trim().strip_prefix(',').unwrap_or(rest.trim()).trim();

    if effect_text.is_empty() {
        return None;
    }

    // Reconstruct the effect text using the original (non-lowered) line
    let effect_start = line.len() - effect_text.len();
    let original_effect = line[effect_start..].trim();

    let execute = parse_effect_chain(original_effect, AbilityKind::Spell);

    let _ = card_name; // used in is_class_level_trigger, not needed here

    Some(
        TriggerDefinition::new(TriggerMode::ClassLevelGained)
            .valid_card(TargetFilter::SelfRef)
            .execute(execute)
            .trigger_zones(vec![Zone::Battlefield])
            .constraint(TriggerConstraint::AtClassLevel { level })
            .description(format!("When this Class becomes level {level}")),
    )
}

/// Wrap a static definition's condition with ClassLevelGE.
/// If the static already has a condition, compose with And.
fn wrap_static_with_class_level(mut static_def: StaticDefinition, level: u8) -> StaticDefinition {
    let level_cond = StaticCondition::ClassLevelGE { level };
    static_def.condition = Some(match static_def.condition.take() {
        Some(existing) => StaticCondition::And {
            conditions: vec![level_cond, existing],
        },
        None => level_cond,
    });
    static_def
}
