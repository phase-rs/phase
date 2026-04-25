use nom::bytes::complete::tag;
use nom::Parser;
use nom_language::error::VerboseError;

use crate::types::ability::{
    AbilityDefinition, AbilityKind, ActivationRestriction, Effect, StaticCondition,
    StaticDefinition, TargetFilter, TriggerCondition, TriggerConstraint, TriggerDefinition,
};
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::oracle::{has_unimplemented, make_unimplemented, ParsedAbilities};
use super::oracle_classifier::{
    is_effect_sentence_candidate, is_granted_static_line, is_replacement_pattern, is_static_pattern,
};
use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::parse_effect_chain;
use super::oracle_keyword::extract_keyword_line;
use super::oracle_modal::strip_ability_word;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_replacement::parse_replacement_line;
use super::oracle_special::normalize_self_refs_for_static;
use super::oracle_static::parse_static_line;
use super::oracle_trigger::parse_trigger_lines_at_index;
use super::oracle_util::{strip_reminder_text, TextPair};

/// Detect a "{cost}: Level N" line using structural parsing.
/// Returns `(level_number, cost_text)` if the line matches.
pub(crate) fn parse_class_level_line(line: &str) -> Option<(u8, String)> {
    let colon_pos = super::oracle::find_activated_colon(line)?;
    let cost_text = line[..colon_pos].trim();
    let effect_text = line[colon_pos + 1..].trim();
    let lower_effect = effect_text.to_lowercase();

    // Check if the effect portion is "Level N" using the shared nom combinator.
    let rest = lower_effect.strip_prefix("level ")?;
    let (remainder, n) = nom_primitives::parse_number(rest).ok()?;
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
                // CR 707.9a: Pass the running trigger count as the base index
                // so any "and it has this ability" except clause inside a
                // Class-level trigger body resolves to the correct printed
                // trigger slot. Without this, level-gated triggers using
                // `RetainPrintedTriggerFromSource` would point at the wrong
                // (or non-existent) source trigger index.
                let mut triggers =
                    parse_trigger_lines_at_index(line, card_name, Some(result.triggers.len()));
                // CR 716.2a: Gate continuous triggers at levels > 1.
                if section.level > 1 {
                    for trigger in &mut triggers {
                        trigger.condition = Some(TriggerCondition::ClassLevelGE {
                            level: section.level,
                        });
                    }
                }
                result.triggers.extend(triggers);
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
                    // they fire at all levels once added. This matches CR 716.2a.
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
                    // CR 707.9a: Same trigger-index threading as the bare
                    // trigger arm above — required for the "has this ability"
                    // retain modification to point at the correct source slot.
                    let mut triggers = parse_trigger_lines_at_index(
                        &effect_text,
                        card_name,
                        Some(result.triggers.len()),
                    );
                    if section.level > 1 {
                        for trigger in &mut triggers {
                            trigger.condition = Some(TriggerCondition::ClassLevelGE {
                                level: section.level,
                            });
                        }
                    }
                    result.triggers.extend(triggers);
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

/// Check if a line matches "when ~ becomes level N" pattern.
///
/// Subject is `~` after `parse_oracle_text` normalizes self-references
/// (CR 201.4b) — `this class` and the bare card name both fold to `~`.
/// The `this class` / card-name fallbacks remain for callers that bypass the
/// parser entry point (e.g. direct tests passing pre-normalization text).
pub(crate) fn is_class_level_trigger(lower: &str, card_name: &str) -> bool {
    // Prefix: CR 603 trigger phrase "when ".
    let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("when ").parse(lower) else {
        return false;
    };
    // Required body phrase "becomes level ".
    if !nom_primitives::scan_contains(rest, "becomes level ") {
        return false;
    }
    // Subject must be `~`, `this class`, or the (non-empty) card name.
    // Guard against empty card_name — `str::contains("")` is universally true
    // and would make the third branch match every line.
    let card_lower = card_name.to_lowercase();
    nom_primitives::scan_contains(rest, "~")
        || nom_primitives::scan_contains(rest, "this class")
        || (!card_lower.is_empty() && nom_primitives::scan_contains(rest, &card_lower))
}

/// Parse a "When this Class becomes level N, {effect}" trigger.
fn parse_class_level_trigger(line: &str, card_name: &str, level: u8) -> Option<TriggerDefinition> {
    // Find "becomes level N" and extract the effect after the comma (case-insensitive)
    let lower = line.to_lowercase();
    let tp = TextPair::new(line, &lower);
    let after_becomes = tp.strip_after("becomes level ")?.original;

    // Parse the level number using the shared nom combinator.
    let after_lower = after_becomes.to_lowercase();
    let (rest, _) = nom_primitives::parse_number(&after_lower).ok()?;

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
            .description(format!("When ~ becomes level {level}")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ContinuousModification, Effect};

    /// CR 707.9a + CR 716.2a: A Class-level trigger body using "becomes a copy
    /// of <X>, except <pronoun> has this ability" must emit
    /// `RetainPrintedTriggerFromSource { source_trigger_index: <N> }` where
    /// `<N>` is the trigger's index in the card's full printed-trigger list.
    /// This guards against the regression where Class-level triggers called
    /// `parse_trigger_lines` (no index thread), causing the retain modification
    /// to either silently drop or point at trigger index 0 regardless of where
    /// the trigger actually sits in the printed list.
    #[test]
    fn class_level_trigger_become_copy_threads_trigger_index() {
        // Synthetic two-level Class enchantment: level 1 has a trigger
        // already (so the level-2 trigger occupies index 1 in the printed
        // list, not index 0). Level 2 introduces a body trigger
        // "At the beginning of your upkeep, ~ becomes a copy of … and
        // it has this ability".
        //
        // Without the index thread, `RetainPrintedTriggerFromSource` would
        // come out as `source_trigger_index: 0`, pointing at the wrong
        // trigger. With the thread, it correctly points at index 1.
        //
        // The level-2 body uses a phase trigger (At the beginning of …)
        // rather than the class-level `When ~ becomes level N` trigger,
        // because the latter takes a special path that doesn't dispatch
        // to the chain parser for the body — the AtClassLevel trigger is
        // a registration-time event, not a body-effect trigger.
        let lines = vec![
            "When this Class enters, draw a card.",
            "{2}: Level 2",
            "At the beginning of your upkeep, ~ becomes a copy of target creature you control, except its name is ~ and it has this ability.",
        ];
        let result = parse_class_oracle_text(
            &lines,
            "Test Class",
            &[],
            ParsedAbilities {
                abilities: Vec::new(),
                triggers: Vec::new(),
                statics: Vec::new(),
                replacements: Vec::new(),
                extracted_keywords: Vec::new(),
                modal: None,
                additional_cost: None,
                casting_restrictions: Vec::new(),
                casting_options: Vec::new(),
                solve_condition: None,
                strive_cost: None,
                parse_warnings: Vec::new(),
            },
        );

        // Find the level-2 BecomeCopy trigger.
        let become_copy_trigger = result
            .triggers
            .iter()
            .find(|t| {
                t.execute
                    .as_ref()
                    .is_some_and(|e| matches!(*e.effect, Effect::BecomeCopy { .. }))
            })
            .expect("level-2 trigger should produce a BecomeCopy effect");

        // The trigger's index in the printed list — should be 1 (the level-1
        // ETB trigger occupies index 0).
        let expected_index = result
            .triggers
            .iter()
            .position(|t| std::ptr::eq(t, become_copy_trigger))
            .unwrap();
        assert_eq!(
            expected_index, 1,
            "level-2 BecomeCopy trigger must occupy index 1 (level-1 ETB at 0); \
             test setup is wrong if this fails"
        );

        // The retain modification's source_trigger_index must match.
        let execute = become_copy_trigger.execute.as_deref().unwrap();
        match execute.effect.as_ref() {
            Effect::BecomeCopy {
                additional_modifications,
                ..
            } => {
                let retain = additional_modifications
                    .iter()
                    .find_map(|m| match m {
                        ContinuousModification::RetainPrintedTriggerFromSource {
                            source_trigger_index,
                        } => Some(*source_trigger_index),
                        _ => None,
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "level-2 BecomeCopy must include a \
                             RetainPrintedTriggerFromSource modification; \
                             got {additional_modifications:?}"
                        )
                    });
                // CR 707.9a: the retained-trigger index must equal this trigger's
                // position in the printed list — guards against the regression where
                // Class-level triggers don't thread the index.
                assert_eq!(
                    retain, expected_index,
                    "CR 707.9a: retained-trigger index must equal this trigger's \
                     position in the printed list ({expected_index}); got {retain}"
                );
            }
            other => panic!("expected BecomeCopy, got {other:?}"),
        }
    }
}
