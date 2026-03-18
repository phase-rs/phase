use serde::{Deserialize, Serialize};

use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, AdditionalCost,
    CastingRestriction, Comparator, CounterTriggerFilter, Effect, ModalChoice,
    ModalSelectionConstraint, ReplacementDefinition, SolveCondition, SpellCastingOption,
    StaticCondition, StaticDefinition, TargetFilter, TriggerCondition, TriggerConstraint,
    TriggerDefinition, TypedFilter,
};
use crate::types::keywords::Keyword;
use crate::types::replacements::ReplacementEvent;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::parse_effect_chain;
use super::oracle_replacement::parse_replacement_line;
use super::oracle_static::parse_static_line;
use super::oracle_trigger::parse_trigger_line;
use super::oracle_util::{parse_mana_symbols, strip_reminder_text};

/// Collected parsed abilities from Oracle text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedAbilities {
    pub abilities: Vec<AbilityDefinition>,
    pub triggers: Vec<TriggerDefinition>,
    pub statics: Vec<StaticDefinition>,
    pub replacements: Vec<ReplacementDefinition>,
    /// Keywords extracted from Oracle text keyword-only lines (e.g. "Protection from multicolored").
    /// Merged with MTGJSON keywords in the loader to form the complete keyword set.
    pub extracted_keywords: Vec<Keyword>,
    /// Modal spell metadata, set when Oracle text begins with "Choose one —" etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal: Option<ModalChoice>,
    /// Additional casting cost parsed from "As an additional cost..." text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_cost: Option<AdditionalCost>,
    /// Spell-casting restrictions parsed from Oracle text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casting_restrictions: Vec<CastingRestriction>,
    /// Spell-casting options parsed from Oracle text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casting_options: Vec<SpellCastingOption>,
    /// CR 719.1: Solve condition for Case enchantments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solve_condition: Option<SolveCondition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OracleBlockAst {
    ActivatedModal {
        cost_text: String,
        header: ModalHeaderAst,
        modes: Vec<ModeAst>,
    },
    Modal {
        header: ModalHeaderAst,
        modes: Vec<ModeAst>,
    },
    TriggeredModal {
        trigger_line: String,
        header: ModalHeaderAst,
        modes: Vec<ModeAst>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModeAst {
    raw: String,
    label: Option<String>,
    body: String,
    /// Per-mode additional cost (Spree). None for standard `•` modes.
    mode_cost: Option<crate::types::mana::ManaCost>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModalHeaderAst {
    raw: String,
    min_choices: usize,
    max_choices: usize,
    allow_repeat_modes: bool,
    constraints: Vec<ModalSelectionConstraint>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ActivatedConstraintAst {
    restrictions: Vec<ActivationRestriction>,
}

impl ActivatedConstraintAst {
    fn sorcery_speed(&self) -> bool {
        self.restrictions
            .contains(&ActivationRestriction::AsSorcery)
    }
}

/// Parse Oracle text into structured ability definitions.
///
/// Splits on newlines, strips reminder text, then classifies each line
/// according to a priority table (keywords, enchant, equip, activated,
/// triggered, static, replacement, spell effect, modal, loyalty, etc.).
///
/// `mtgjson_keyword_names` are the raw lowercased keyword names from MTGJSON
/// (e.g. `["flying", "protection"]`). Used to identify keyword-only lines
/// and to avoid re-extracting keywords MTGJSON already provides.
pub fn parse_oracle_text(
    oracle_text: &str,
    card_name: &str,
    mtgjson_keyword_names: &[String],
    types: &[String],
    subtypes: &[String],
) -> ParsedAbilities {
    let is_spell = types.iter().any(|t| t == "Instant" || t == "Sorcery");

    let mut result = ParsedAbilities {
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
    };

    let lines: Vec<&str> = oracle_text.split('\n').collect();

    // CR 714: Pre-parse Saga chapter lines into triggers + ETB replacement.
    if subtypes.iter().any(|s| s == "Saga") {
        let (chapter_triggers, etb_replacement) = parse_saga_chapters(&lines, card_name);
        result.triggers.extend(chapter_triggers);
        result.replacements.push(etb_replacement);
    }

    // CR 716: Pre-parse Class level sections into level-gated abilities.
    if subtypes.iter().any(|s| s == "Class") {
        return parse_class_oracle_text(&lines, card_name, mtgjson_keyword_names, result);
    }

    let mut i = 0;

    while i < lines.len() {
        let raw_line = lines[i].trim();
        if raw_line.is_empty() {
            i += 1;
            continue;
        }

        let line = strip_reminder_text(raw_line);
        if line.is_empty() {
            // Priority 14: entirely parenthesized reminder text
            i += 1;
            continue;
        }

        // Priority 1: Modal block (standard "Choose one —" + modes, or Spree + modes).
        // Must run before keyword extraction so "Spree" header + follow-on `+` lines
        // are consumed as a modal block, not swallowed as a keyword-only line.
        if let Some((block, next_i)) = parse_oracle_block(&lines, i) {
            lower_oracle_block(block, card_name, &mut result);
            i = next_i;
            continue;
        }

        // Priority 1b: keyword-only line — extract any keywords for the union set
        if let Some(extracted) = extract_keyword_line(&line, mtgjson_keyword_names) {
            result.extracted_keywords.extend(extracted);
            i += 1;
            continue;
        }

        let lower = line.to_lowercase();

        // Normalize card self-references for static parsing (replace card name with ~)
        let static_line = normalize_self_refs_for_static(&line, card_name);

        // Priority 2: "Enchant {filter}" — skip (handled externally)
        if lower.starts_with("enchant ") && !lower.starts_with("enchanted ") {
            i += 1;
            continue;
        }

        // Priority 3: "Equip {cost}" / "Equip — {cost}" (but not "Equipped ...")
        if lower.starts_with("equip") && !lower.starts_with("equipped") {
            if let Some(ability) = try_parse_equip(&line) {
                result.abilities.push(ability);
                i += 1;
                continue;
            }
        }
        // Priority 11: Planeswalker loyalty abilities: +N:, −N:, 0:, [+N]:, [−N]:, [0]:
        if let Some(ability) = try_parse_loyalty_line(&line) {
            result.abilities.push(ability);
            i += 1;
            continue;
        }

        if is_granted_static_line(&lower) {
            if let Some(static_def) = parse_static_line(&static_line) {
                result.statics.push(static_def);
                i += 1;
                continue;
            }
        }

        // Priority 3b: Case "To solve — {condition}" line (CR 719.1)
        if let Some(rest) = lower
            .strip_prefix("to solve — ")
            .or_else(|| lower.strip_prefix("to solve -- "))
        {
            result.solve_condition = Some(parse_solve_condition(rest));
            i += 1;
            continue;
        }

        // CR 719.4: Case "Solved — {cost}: {effect}" activated ability.
        if let Some(rest) = line
            .strip_prefix("Solved — ")
            .or_else(|| line.strip_prefix("Solved -- "))
        {
            if let Some(colon_pos) = find_activated_colon(rest) {
                let cost_text = rest[..colon_pos].trim();
                let effect_text = rest[colon_pos + 1..].trim();
                let (effect_text, constraints) = strip_activated_constraints(effect_text);
                let cost = parse_oracle_cost(cost_text);

                let mut def = parse_effect_chain(&effect_text, AbilityKind::Activated);
                def.cost = Some(cost);
                def.description = Some(line.to_string());
                // CR 719.4: Solved abilities only activate while Case is solved.
                def.activation_restrictions
                    .push(ActivationRestriction::IsSolved);
                if constraints.sorcery_speed() {
                    def.sorcery_speed = true;
                    def.activation_restrictions
                        .push(ActivationRestriction::AsSorcery);
                }
                if !constraints.restrictions.is_empty() {
                    def.activation_restrictions.extend(constraints.restrictions);
                }
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 4: Activated ability — contains ":" with cost-like prefix
        if let Some(colon_pos) = find_activated_colon(&line) {
            let cost_text = line[..colon_pos].trim();
            let effect_text = line[colon_pos + 1..].trim();
            let (effect_text, constraints) = strip_activated_constraints(effect_text);
            let cost = parse_oracle_cost(cost_text);

            let mut def = parse_effect_chain(&effect_text, AbilityKind::Activated);
            def.cost = Some(cost);
            def.description = Some(line.to_string());
            if constraints.sorcery_speed() {
                def.sorcery_speed = true;
            }
            if !constraints.restrictions.is_empty() {
                def.activation_restrictions = constraints.restrictions;
            }
            result.abilities.push(def);
            i += 1;
            continue;
        }

        // Priority 5-6: Triggered abilities — starts with When/Whenever/At
        if lower.starts_with("when ") || lower.starts_with("whenever ") || lower.starts_with("at ")
        {
            let trigger = parse_trigger_line(&line, card_name);
            result.triggers.push(trigger);
            i += 1;
            continue;
        }

        // Priority 7: Static/continuous patterns
        if is_static_pattern(&lower) {
            if let Some(static_def) = parse_static_line(&static_line) {
                result.statics.push(static_def);
                i += 1;
                continue;
            }
        }

        // Priority 8: Replacement patterns
        if is_replacement_pattern(&lower) {
            if let Some(rep_def) = parse_replacement_line(&line, card_name) {
                result.replacements.push(rep_def);
                i += 1;
                continue;
            }
        }

        // Priority 8c: "If this card is in your opening hand, you may begin the game with it on the battlefield"
        if lower.contains("opening hand") && lower.contains("begin the game") {
            result.abilities.push(
                AbilityDefinition::new(
                    AbilityKind::BeginGame,
                    Effect::ChangeZone {
                        destination: crate::types::zones::Zone::Battlefield,
                        target: crate::types::ability::TargetFilter::SelfRef,
                        origin: Some(crate::types::zones::Zone::Hand),
                        owner_library: false,
                    },
                )
                .description(line.to_string()),
            );
            i += 1;
            continue;
        }

        // Priority 8b: "As an additional cost to cast this spell"
        if lower.starts_with("as an additional cost") {
            result.additional_cost = parse_additional_cost_line(&lower, &line);
            i += 1;
            continue;
        }

        if is_spell {
            if let Some(option) = parse_spell_casting_option_line(&line, card_name) {
                result.casting_options.push(option);
                i += 1;
                continue;
            }
            if let Some(restrictions) = parse_casting_restriction_line(&line) {
                result.casting_restrictions.extend(restrictions);
                i += 1;
                continue;
            }
        }

        // Priority 9: Imperative verb for instants/sorceries
        if is_spell {
            let mut def = parse_effect_chain(&line, AbilityKind::Spell);
            def.description = Some(line.to_string());
            result.abilities.push(def);
            i += 1;
            continue;
        }

        // Priority 12: Roman numeral chapters (saga) — skip
        if is_saga_chapter(&lower) {
            i += 1;
            continue;
        }

        // Priority 13: Keyword cost lines — skip (handled by MTGJSON keywords)
        if is_keyword_cost_line(&lower) {
            i += 1;
            continue;
        }

        // Priority 13b: Kicker/Multikicker — skip (handled by keywords)
        if lower.starts_with("kicker") || lower.starts_with("multikicker") {
            i += 1;
            continue;
        }

        // Priority 13c: "Activate only..." constraint — skip
        if lower.starts_with("activate ") || lower.starts_with("activate only") {
            i += 1;
            continue;
        }

        // Priority 14: Ability word — strip prefix and re-classify effect
        if let Some(effect_text) = strip_ability_word(&line) {
            let effect_lower = effect_text.to_lowercase();
            // Try as trigger
            if effect_lower.starts_with("when ")
                || effect_lower.starts_with("whenever ")
                || effect_lower.starts_with("at ")
            {
                let trigger = parse_trigger_line(&effect_text, card_name);
                result.triggers.push(trigger);
                i += 1;
                continue;
            }
            // Try as static
            if is_static_pattern(&effect_lower) {
                let effect_static = normalize_self_refs_for_static(&effect_text, card_name);
                if let Some(static_def) = parse_static_line(&effect_static) {
                    result.statics.push(static_def);
                    i += 1;
                    continue;
                }
            }
            // Try as effect
            let def = parse_effect_chain(&effect_text, AbilityKind::Spell);
            if !has_unimplemented(&def) {
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 14a: "damage can't be prevented" → AddRestriction effect
        if lower.contains("damage") && lower.contains("can't be prevented") {
            let def = parse_effect_chain(&line, AbilityKind::Spell);
            if !has_unimplemented(&def) {
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 14b: Try parsing as effect even for non-spells
        if is_effect_sentence_candidate(&lower) {
            let def = parse_effect_chain(&line, AbilityKind::Spell);
            if !has_unimplemented(&def) {
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 15: Fallback
        result.abilities.push(make_unimplemented(&line));
        i += 1;
    }

    result
}

// ---------------------------------------------------------------------------
// Class enchantment parser (CR 716)
// ---------------------------------------------------------------------------

/// Detect a "{cost}: Level N" line using structural parsing.
/// Returns `(level_number, cost_text)` if the line matches.
fn parse_class_level_line(line: &str) -> Option<(u8, String)> {
    let colon_pos = find_activated_colon(line)?;
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
fn parse_class_oracle_text(
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
fn is_class_level_trigger(lower: &str, card_name: &str) -> bool {
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

/// Try to extract keywords from a keyword-only line (comma-separated).
/// Returns `Some(keywords)` if the entire line consists of recognizable keywords
/// AND at least one part matches an MTGJSON keyword name (preventing false positives
/// from standalone ability lines like "Equip {1}").
///
/// Returns only keywords not already covered by MTGJSON names — these are typically
/// parameterized keywords where MTGJSON lists the name (e.g. "Protection") but
/// Oracle text has the full form (e.g. "Protection from multicolored").
fn extract_keyword_line(line: &str, mtgjson_keyword_names: &[String]) -> Option<Vec<Keyword>> {
    if mtgjson_keyword_names.is_empty() {
        return None;
    }

    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.is_empty() {
        return None;
    }

    let mut any_mtgjson_match = false;
    let mut new_keywords = Vec::new();

    for part in &parts {
        let lower = part.to_lowercase();

        // Check if this part matches or extends an MTGJSON keyword name.
        // Exact match: "flying" == "flying"
        // Prefix match: "protection from multicolored" starts with "protection"
        let mtgjson_match = mtgjson_keyword_names
            .iter()
            .any(|name| lower == *name || lower.starts_with(&format!("{name} ")));

        if mtgjson_match {
            any_mtgjson_match = true;

            // Exact name match means MTGJSON already has the parsed keyword — skip
            if mtgjson_keyword_names.contains(&lower) {
                continue;
            }

            // Prefix match: Oracle text has more detail (e.g. "protection from red").
            // Extract the full parameterized keyword.
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                new_keywords.push(kw);
                continue;
            }
        }

        // Not an MTGJSON match — try parsing as any keyword (for keyword-only line validation)
        if let Some(kw) = parse_keyword_from_oracle(&lower) {
            if !matches!(kw, Keyword::Unknown(_)) {
                continue;
            }
        }

        // Unrecognized part — not a keyword line
        return None;
    }

    if any_mtgjson_match {
        Some(new_keywords)
    } else {
        None
    }
}

/// Parse a keyword from Oracle text format (natural language) into a `Keyword`.
///
/// Oracle text uses space-separated format: "protection from red", "ward {2}",
/// "flashback {2}{U}". Converts to the colon format that `FromStr` expects,
/// handling the "from" preposition used by protection keywords.
fn parse_keyword_from_oracle(text: &str) -> Option<Keyword> {
    // First try direct parse (handles simple keywords like "flying")
    let direct: Keyword = text.parse().unwrap();
    if !matches!(direct, Keyword::Unknown(_)) {
        return Some(direct);
    }

    // For parameterized keywords, find the first space to split name from parameter.
    // Oracle format: "protection from multicolored" → name="protection", rest="from multicolored"
    // Oracle format: "ward {2}" → name="ward", rest="{2}"
    let space_idx = text.find(' ')?;
    let name = &text[..space_idx];
    let rest = text[space_idx + 1..].trim();

    // Strip "from" preposition (used by protection keywords)
    let param = rest.strip_prefix("from ").unwrap_or(rest);

    let colon_form = format!("{name}:{param}");
    let parsed: Keyword = colon_form.parse().unwrap();
    if matches!(parsed, Keyword::Unknown(_)) {
        return None;
    }
    Some(parsed)
}

/// Get a lowercase display name for a keyword variant.
pub fn keyword_display_name(keyword: &Keyword) -> String {
    match keyword {
        Keyword::Flying => "flying".to_string(),
        Keyword::FirstStrike => "first strike".to_string(),
        Keyword::DoubleStrike => "double strike".to_string(),
        Keyword::Trample => "trample".to_string(),
        Keyword::Deathtouch => "deathtouch".to_string(),
        Keyword::Lifelink => "lifelink".to_string(),
        Keyword::Vigilance => "vigilance".to_string(),
        Keyword::Haste => "haste".to_string(),
        Keyword::Reach => "reach".to_string(),
        Keyword::Defender => "defender".to_string(),
        Keyword::Menace => "menace".to_string(),
        Keyword::Indestructible => "indestructible".to_string(),
        Keyword::Hexproof => "hexproof".to_string(),
        Keyword::Shroud => "shroud".to_string(),
        Keyword::Flash => "flash".to_string(),
        Keyword::Fear => "fear".to_string(),
        Keyword::Intimidate => "intimidate".to_string(),
        Keyword::Skulk => "skulk".to_string(),
        Keyword::Shadow => "shadow".to_string(),
        Keyword::Horsemanship => "horsemanship".to_string(),
        Keyword::Wither => "wither".to_string(),
        Keyword::Infect => "infect".to_string(),
        Keyword::Afflict => "afflict".to_string(),
        Keyword::Prowess => "prowess".to_string(),
        Keyword::Undying => "undying".to_string(),
        Keyword::Persist => "persist".to_string(),
        Keyword::Cascade => "cascade".to_string(),
        Keyword::Convoke => "convoke".to_string(),
        Keyword::Delve => "delve".to_string(),
        Keyword::Devoid => "devoid".to_string(),
        Keyword::Exalted => "exalted".to_string(),
        Keyword::Flanking => "flanking".to_string(),
        Keyword::Changeling => "changeling".to_string(),
        Keyword::Phasing => "phasing".to_string(),
        Keyword::Battlecry => "battlecry".to_string(),
        Keyword::Decayed => "decayed".to_string(),
        Keyword::Unleash => "unleash".to_string(),
        Keyword::Riot => "riot".to_string(),
        Keyword::LivingWeapon => "living weapon".to_string(),
        Keyword::TotemArmor => "totem armor".to_string(),
        Keyword::Evolve => "evolve".to_string(),
        Keyword::Extort => "extort".to_string(),
        Keyword::Exploit => "exploit".to_string(),
        Keyword::Explore => "explore".to_string(),
        Keyword::Ascend => "ascend".to_string(),
        Keyword::Soulbond => "soulbond".to_string(),
        Keyword::Banding => "banding".to_string(),
        Keyword::Cumulative => "cumulative".to_string(),
        Keyword::Epic => "epic".to_string(),
        Keyword::Fuse => "fuse".to_string(),
        Keyword::Gravestorm => "gravestorm".to_string(),
        Keyword::Haunt => "haunt".to_string(),
        Keyword::Hideaway => "hideaway".to_string(),
        Keyword::Improvise => "improvise".to_string(),
        Keyword::Ingest => "ingest".to_string(),
        Keyword::Melee => "melee".to_string(),
        Keyword::Mentor => "mentor".to_string(),
        Keyword::Myriad => "myriad".to_string(),
        Keyword::Provoke => "provoke".to_string(),
        Keyword::Rebound => "rebound".to_string(),
        Keyword::Retrace => "retrace".to_string(),
        Keyword::Ripple => "ripple".to_string(),
        Keyword::SplitSecond => "split second".to_string(),
        Keyword::Storm => "storm".to_string(),
        Keyword::Suspend => "suspend".to_string(),
        Keyword::Totem => "totem".to_string(),
        Keyword::Warp => "warp".to_string(),
        Keyword::Gift => "gift".to_string(),
        Keyword::Spree => "spree".to_string(),
        Keyword::Ravenous => "ravenous".to_string(),
        Keyword::Daybound => "daybound".to_string(),
        Keyword::Nightbound => "nightbound".to_string(),
        Keyword::Enlist => "enlist".to_string(),
        Keyword::ReadAhead => "read ahead".to_string(),
        Keyword::Compleated => "compleated".to_string(),
        Keyword::Conspire => "conspire".to_string(),
        Keyword::Demonstrate => "demonstrate".to_string(),
        Keyword::Dethrone => "dethrone".to_string(),
        Keyword::DoubleTeam => "double team".to_string(),
        Keyword::LivingMetal => "living metal".to_string(),
        // Parameterized keywords — return just the base name
        Keyword::Dredge(_) => "dredge".to_string(),
        Keyword::Modular(_) => "modular".to_string(),
        Keyword::Renown(_) => "renown".to_string(),
        Keyword::Fabricate(_) => "fabricate".to_string(),
        Keyword::Annihilator(_) => "annihilator".to_string(),
        Keyword::Bushido(_) => "bushido".to_string(),
        Keyword::Tribute(_) => "tribute".to_string(),
        Keyword::Afterlife(_) => "afterlife".to_string(),
        Keyword::Fading(_) => "fading".to_string(),
        Keyword::Vanishing(_) => "vanishing".to_string(),
        Keyword::Rampage(_) => "rampage".to_string(),
        Keyword::Absorb(_) => "absorb".to_string(),
        Keyword::Crew(_) => "crew".to_string(),
        Keyword::Poisonous(_) => "poisonous".to_string(),
        Keyword::Bloodthirst(_) => "bloodthirst".to_string(),
        Keyword::Amplify(_) => "amplify".to_string(),
        Keyword::Graft(_) => "graft".to_string(),
        Keyword::Devour(_) => "devour".to_string(),
        Keyword::Protection(_) => "protection".to_string(),
        Keyword::Kicker(_) => "kicker".to_string(),
        Keyword::Cycling(_) => "cycling".to_string(),
        Keyword::Flashback(_) => "flashback".to_string(),
        Keyword::Ward(_) => "ward".to_string(),
        Keyword::Equip(_) => "equip".to_string(),
        Keyword::Landwalk(_) => "landwalk".to_string(),
        Keyword::Partner(_) => "partner".to_string(),
        Keyword::Companion(_) => "companion".to_string(),
        Keyword::Ninjutsu(_) => "ninjutsu".to_string(),
        Keyword::Enchant(_) => "enchant".to_string(),
        Keyword::EtbCounter { .. } => "etb counter".to_string(),
        Keyword::Reconfigure(_) => "reconfigure".to_string(),
        Keyword::Bestow(_) => "bestow".to_string(),
        Keyword::Embalm(_) => "embalm".to_string(),
        Keyword::Eternalize(_) => "eternalize".to_string(),
        Keyword::Unearth(_) => "unearth".to_string(),
        Keyword::Prowl(_) => "prowl".to_string(),
        Keyword::Morph(_) => "morph".to_string(),
        Keyword::Megamorph(_) => "megamorph".to_string(),
        Keyword::Madness(_) => "madness".to_string(),
        Keyword::Dash(_) => "dash".to_string(),
        Keyword::Emerge(_) => "emerge".to_string(),
        Keyword::Escape(_) => "escape".to_string(),
        Keyword::Evoke(_) => "evoke".to_string(),
        Keyword::Foretell(_) => "foretell".to_string(),
        Keyword::Mutate(_) => "mutate".to_string(),
        Keyword::Disturb(_) => "disturb".to_string(),
        Keyword::Disguise(_) => "disguise".to_string(),
        Keyword::Blitz(_) => "blitz".to_string(),
        Keyword::Overload(_) => "overload".to_string(),
        Keyword::Spectacle(_) => "spectacle".to_string(),
        Keyword::Surge(_) => "surge".to_string(),
        Keyword::Encore(_) => "encore".to_string(),
        Keyword::Buyback(_) => "buyback".to_string(),
        Keyword::Echo(_) => "echo".to_string(),
        Keyword::Outlast(_) => "outlast".to_string(),
        Keyword::Scavenge(_) => "scavenge".to_string(),
        Keyword::Fortify(_) => "fortify".to_string(),
        Keyword::Prototype(_) => "prototype".to_string(),
        Keyword::Plot(_) => "plot".to_string(),
        Keyword::Craft(_) => "craft".to_string(),
        Keyword::Offspring(_) => "offspring".to_string(),
        Keyword::Impending(_) => "impending".to_string(),
        Keyword::Unknown(s) => s.to_lowercase(),
    }
}

/// Try to parse "Equip {cost}" or "Equip — {cost}" lines.
fn try_parse_equip(line: &str) -> Option<AbilityDefinition> {
    let lower = line.to_lowercase();
    if !lower.starts_with("equip") {
        return None;
    }
    let rest = line[5..].trim();
    // Strip leading "—" or "- "
    let cost_text = rest
        .strip_prefix('—')
        .or_else(|| rest.strip_prefix('-'))
        .unwrap_or(rest)
        .trim();

    if cost_text.is_empty() {
        return None;
    }

    let cost = parse_oracle_cost(cost_text);
    Some(
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Attach {
                target: crate::types::ability::TargetFilter::Typed(
                    TypedFilter::creature().controller(crate::types::ability::ControllerRef::You),
                ),
            },
        )
        .cost(cost)
        .description(line.to_string())
        .sorcery_speed(),
    )
}

/// Try to parse a planeswalker loyalty line: "+N:", "−N:", "0:", "[+N]:", "[−N]:", "[0]:"
fn try_parse_loyalty_line(line: &str) -> Option<AbilityDefinition> {
    let trimmed = line.trim();

    // Try bracket format first: [+2]: ..., [−1]: ..., [0]: ...
    if trimmed.starts_with('[') {
        if let Some(bracket_end) = trimmed.find(']') {
            let inner = &trimmed[1..bracket_end];
            let after_bracket = trimmed[bracket_end + 1..].trim();
            if let Some(effect_text) = after_bracket.strip_prefix(':') {
                if let Some(amount) = parse_loyalty_number(inner) {
                    let effect_text = effect_text.trim();
                    let mut def = parse_effect_chain(effect_text, AbilityKind::Activated);
                    def.cost = Some(AbilityCost::Loyalty { amount });
                    def.description = Some(trimmed.to_string());
                    return Some(def);
                }
            }
        }
    }

    // Try bare format: +2: ..., −1: ..., 0: ...
    if let Some(colon_pos) = trimmed.find(':') {
        let prefix = &trimmed[..colon_pos];
        if let Some(amount) = parse_loyalty_number(prefix) {
            // Verify it looks like a loyalty prefix (starts with +, −, –, -, or is "0")
            let first_char = prefix.trim().chars().next()?;
            if first_char == '+'
                || first_char == '−'
                || first_char == '–'
                || first_char == '-'
                || prefix.trim() == "0"
            {
                let effect_text = trimmed[colon_pos + 1..].trim();
                let mut def = parse_effect_chain(effect_text, AbilityKind::Activated);
                def.cost = Some(AbilityCost::Loyalty { amount });
                def.description = Some(trimmed.to_string());
                return Some(def);
            }
        }
    }

    None
}

/// Parse a loyalty number string like "+2", "−3", "0", "-1".
fn parse_loyalty_number(s: &str) -> Option<i32> {
    let s = s.trim();
    // Normalize Unicode minus signs
    let normalized = s.replace(['−', '–'], "-");
    // "+N" → positive
    if let Some(rest) = normalized.strip_prefix('+') {
        return rest.parse::<i32>().ok();
    }
    // "-N" or bare number
    normalized.parse::<i32>().ok()
}

/// Find the position of ":" that indicates an activated ability cost/effect split.
/// The left side must look like a cost (contains "{", or starts with cost-like words,
/// or is a loyalty marker).
fn find_activated_colon(line: &str) -> Option<usize> {
    let colon_pos = find_top_level_colon(line)?;
    let prefix = &line[..colon_pos];
    let lower_prefix = prefix.to_lowercase().trim().to_string();

    // Contains mana symbols
    if prefix.contains('{') {
        return Some(colon_pos);
    }

    // Starts with cost-like words
    let cost_starters = [
        "sacrifice",
        "discard",
        "pay",
        "remove",
        "exile",
        "tap",
        "untap",
    ];
    if cost_starters.iter().any(|s| lower_prefix.starts_with(s)) {
        return Some(colon_pos);
    }

    None
}

fn find_top_level_colon(line: &str) -> Option<usize> {
    let mut paren_depth = 0u32;
    let mut in_quotes = false;

    for (idx, ch) in line.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => paren_depth += 1,
            ')' if !in_quotes => paren_depth = paren_depth.saturating_sub(1),
            ':' if !in_quotes && paren_depth == 0 => return Some(idx),
            _ => {}
        }
    }

    None
}

fn strip_activated_constraints(text: &str) -> (String, ActivatedConstraintAst) {
    let mut remaining = text.trim().trim_end_matches('.').trim().to_string();
    let mut constraints = ActivatedConstraintAst::default();

    'parse_constraints: loop {
        let lower = remaining.to_lowercase();

        for (suffix, parsed) in [
            (
                "activate only as a sorcery and only once each turn",
                vec![
                    ActivationRestriction::AsSorcery,
                    ActivationRestriction::OnlyOnceEachTurn,
                ],
            ),
            (
                "activate only as a sorcery and only once",
                vec![
                    ActivationRestriction::AsSorcery,
                    ActivationRestriction::OnlyOnce,
                ],
            ),
            (
                "activate only during your turn and only once each turn",
                vec![
                    ActivationRestriction::DuringYourTurn,
                    ActivationRestriction::OnlyOnceEachTurn,
                ],
            ),
            (
                "activate only during your upkeep and only once each turn",
                vec![
                    ActivationRestriction::DuringYourUpkeep,
                    ActivationRestriction::OnlyOnceEachTurn,
                ],
            ),
        ] {
            if lower.ends_with(suffix) {
                let end = remaining.len() - suffix.len();
                remaining = remaining[..end]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                constraints.restrictions.extend(parsed);
                if remaining.is_empty() {
                    break 'parse_constraints;
                }
                continue 'parse_constraints;
            }
        }

        if let Some(prefix) = lower.strip_suffix("activate only as a sorcery") {
            let end = remaining.len() - "activate only as a sorcery".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::AsSorcery);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate only as an instant") {
            let end = remaining.len() - "activate only as an instant".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::AsInstant);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate only during your turn") {
            let end = remaining.len() - "activate only during your turn".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::DuringYourTurn);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate only during your upkeep") {
            let end = remaining.len() - "activate only during your upkeep".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::DuringYourUpkeep);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate only during combat") {
            let end = remaining.len() - "activate only during combat".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::DuringCombat);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) =
            lower.strip_suffix("activate only during your turn, before attackers are declared")
        {
            let end = remaining.len()
                - "activate only during your turn, before attackers are declared".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::DuringYourTurn);
            constraints
                .restrictions
                .push(ActivationRestriction::BeforeAttackersDeclared);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) =
            lower.strip_suffix("activate only during combat before combat damage has been dealt")
        {
            let end = remaining.len()
                - "activate only during combat before combat damage has been dealt".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::DuringCombat);
            constraints
                .restrictions
                .push(ActivationRestriction::BeforeCombatDamage);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate only once each turn") {
            let end = remaining.len() - "activate only once each turn".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::OnlyOnceEachTurn);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate only once") {
            let end = remaining.len() - "activate only once".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::OnlyOnce);
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate no more than twice each turn") {
            let end = remaining.len() - "activate no more than twice each turn".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::MaxTimesEachTurn { count: 2 });
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(prefix) = lower.strip_suffix("activate no more than three times each turn") {
            let end = remaining.len() - "activate no more than three times each turn".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints
                .restrictions
                .push(ActivationRestriction::MaxTimesEachTurn { count: 3 });
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

        if let Some(idx) = lower.rfind("activate only if ") {
            if idx == 0 {
                let condition_text = remaining["activate only if ".len()..].trim().to_string();
                remaining.clear();
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        text: condition_text,
                    });
                break;
            }
            if lower[..idx].ends_with(". ") {
                let condition_text = remaining[idx + "activate only if ".len()..]
                    .trim()
                    .to_string();
                remaining = remaining[..idx]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        text: condition_text,
                    });
                continue;
            }
        }

        if let Some(idx) = lower.rfind("activate only from ") {
            if idx == 0 || lower[..idx].ends_with(". ") {
                let restriction_text = remaining[idx + "activate only from ".len()..]
                    .trim()
                    .to_string();
                remaining = remaining[..idx]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        text: format!("from {restriction_text}"),
                    });
                continue;
            }
        }

        if let Some(idx) = lower.rfind("activate only ") {
            if idx == 0 || lower[..idx].ends_with(". ") {
                let restriction_text = remaining[idx + "activate only ".len()..].trim().to_string();
                remaining = remaining[..idx]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        text: restriction_text,
                    });
                continue;
            }
        }

        if let Some(idx) = lower.rfind("activate no more than ") {
            if idx == 0 || lower[..idx].ends_with(". ") {
                let restriction_text = remaining[idx + "activate no more than ".len()..]
                    .trim()
                    .to_string();
                remaining = remaining[..idx]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        text: format!("no more than {restriction_text}"),
                    });
                continue;
            }
        }

        break;
    }

    (remaining, constraints)
}

/// Check if a line looks like a static/continuous ability.
fn is_static_pattern(lower: &str) -> bool {
    lower.contains("gets +")
        || lower.contains("gets -")
        || lower.contains("get +")
        || lower.contains("get -")
        || lower.contains("have ")
        || lower.contains("has ")
        || lower.contains("can't be blocked")
        || lower.contains("can't attack")
        || lower.contains("can't block")
        || lower.contains("can't be countered")
        || lower.contains("can't be the target")
        || lower.contains("can't be sacrificed")
        || lower.contains("doesn't untap")
        || lower.contains("don't untap")
        || lower.contains("attacks each combat if able")
        || lower.contains("can block only creatures with flying")
        || lower.contains("no maximum hand size")
        || lower.contains("may choose not to untap")
        || lower.starts_with("as long as ")
        || lower.starts_with("enchanted ")
        || lower.starts_with("equipped ")
        || lower.starts_with("all creatures ")
        || lower.starts_with("all permanents ")
        || lower.starts_with("other ")
        || lower.starts_with("each creature ")
        || lower.starts_with("cards in ")
        || lower.starts_with("creatures you control ")
        || (lower.starts_with("creatures your opponents control ")
            && !lower.trim_end_matches('.').ends_with("enter tapped"))
        || lower.starts_with("each player ")
        || lower.starts_with("spells you cast ")
        || lower.starts_with("spells your opponents cast ")
        || lower.starts_with("you may look at the top card of your library")
        || (lower.contains("enters with ") && !lower.contains("counter"))
        || lower.contains("cost {")
        || lower.contains("costs {")
        || lower.contains("cost less")
        || lower.contains("cost more")
        || lower.contains("costs less")
        || lower.contains("costs more")
        || lower.contains("is the chosen type")
        || lower.contains("lose all abilities")
        || lower.contains("power is equal to")
        || lower.contains("power and toughness are each equal to")
}

fn is_granted_static_line(lower: &str) -> bool {
    (lower.starts_with("enchanted ")
        || lower.starts_with("equipped ")
        || lower.starts_with("all ")
        || lower.starts_with("creatures ")
        || lower.starts_with("lands ")
        || lower.starts_with("other ")
        || lower.starts_with("you ")
        || lower.starts_with("players ")
        || lower.starts_with("each player "))
        && (lower.contains(" has \"")
            || lower.contains(" have \"")
            || lower.contains(" gains \"")
            || lower.contains(" gain \""))
}

/// Check if a line looks like a replacement effect.
fn is_replacement_pattern(lower: &str) -> bool {
    lower.contains("would ")
        || lower.contains("prevent all")
        // "can't be prevented" is routed to effect parsing (Effect::AddRestriction),
        // not replacement parsing. It disables prevention rather than replacing events.
        || lower.contains("enters the battlefield tapped")
        || lower.contains("enters tapped")
        || lower.trim_end_matches('.').ends_with(" enter tapped")
        || (lower.contains("as ") && lower.contains("enters") && lower.contains("choose a"))
        || (lower.contains("enters") && lower.contains("counter"))
}

/// Parse a roman numeral to u32. Handles I(1) through X(10) and beyond.
fn parse_roman_numeral(s: &str) -> Option<u32> {
    match s.to_uppercase().as_str() {
        "I" => Some(1),
        "II" => Some(2),
        "III" => Some(3),
        "IV" => Some(4),
        "V" => Some(5),
        "VI" => Some(6),
        "VII" => Some(7),
        "VIII" => Some(8),
        "IX" => Some(9),
        "X" => Some(10),
        _ => None,
    }
}

/// Parse a saga chapter line. Returns (chapter_numbers, effect_text).
/// Handles "I — effect", "I, II — effect", "III, IV, V — effect" (arbitrary-length lists).
fn parse_chapter_line(line: &str) -> Option<(Vec<u32>, String)> {
    // Split on em dash or hyphen
    let (prefix, effect) = line.split_once(" — ").or_else(|| line.split_once(" - "))?;

    let nums: Vec<u32> = prefix
        .split(',')
        .filter_map(|part| parse_roman_numeral(part.trim()))
        .collect();

    if nums.is_empty() {
        return None;
    }

    Some((nums, effect.trim().to_string()))
}

/// CR 714: Parse all chapter lines from a Saga's Oracle text.
/// Returns (chapter_triggers, etb_replacement).
fn parse_saga_chapters(
    lines: &[&str],
    _card_name: &str,
) -> (Vec<TriggerDefinition>, ReplacementDefinition) {
    let mut chapters: Vec<(Vec<u32>, String)> = Vec::new();

    for &line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let stripped = strip_reminder_text(trimmed);
        if stripped.is_empty() {
            continue;
        }

        if let Some((nums, effect)) = parse_chapter_line(&stripped) {
            chapters.push((nums, effect));
        } else if let Some(last) = chapters.last_mut() {
            // Multi-line chapter body: continuation of previous chapter
            last.1.push(' ');
            last.1.push_str(&stripped);
        }
    }

    let mut triggers = Vec::new();
    for (nums, effect_text) in &chapters {
        for &n in nums {
            let trigger = TriggerDefinition::new(TriggerMode::CounterAdded)
                .valid_card(TargetFilter::SelfRef)
                .counter_filter(CounterTriggerFilter {
                    counter_type: crate::game::game_object::CounterType::Lore,
                    threshold: Some(n),
                })
                .execute(parse_effect_chain(effect_text, AbilityKind::Spell))
                .trigger_zones(vec![Zone::Battlefield])
                .description(format!("Chapter {n}"));
            triggers.push(trigger);
        }
    }

    // CR 714.3a: As a Saga enters the battlefield, its controller puts a lore counter on it.
    let etb_replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: "lore".to_string(),
                count: 1,
                target: TargetFilter::SelfRef,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .description("Saga ETB lore counter".to_string());

    (triggers, etb_replacement)
}

/// Check if a line is a saga chapter (e.g. "I —", "II —", "III —").
fn is_saga_chapter(lower: &str) -> bool {
    parse_chapter_line(lower).is_some()
}

/// Check if a line is a keyword with a cost (e.g., "Cycling {2}", "Flashback {3}{R}", "Crew 3").
/// These are handled by MTGJSON keywords and should be skipped by the Oracle parser.
fn is_keyword_cost_line(lower: &str) -> bool {
    let keyword_costs = [
        "cycling",
        "flashback",
        "crew",
        "ward",
        "equip", // already handled earlier but as safety
        "bestow",
        "embalm",
        "eternalize",
        "unearth",
        "ninjutsu",
        "prowl",
        "morph",
        "megamorph",
        "madness",
        "dash",
        "emerge",
        "escape",
        "evoke",
        "foretell",
        "mutate",
        "disturb",
        "disguise",
        "blitz",
        "overload",
        "spectacle",
        "surge",
        "encore",
        "buyback",
        "echo",
        "outlast",
        "scavenge",
        "fortify",
        "prototype",
        "plot",
        "craft",
        "offspring",
        "impending",
        "reconfigure",
        "suspend",
        "cumulative upkeep",
        "level up",
        "channel",
        "transfigure",
        "transmute",
        "forecast",
        "recover",
        "reinforce",
        "retrace",
        "adapt",
        "monstrosity",
        "affinity",
        "convoke",
        "delve",
        "improvise",
        "miracle",
        "splice",
        "entwine",
    ];
    keyword_costs.iter().any(|kw| {
        lower.starts_with(kw)
            && (lower.len() == kw.len()
                || lower.as_bytes().get(kw.len()) == Some(&b' ')
                || lower.as_bytes().get(kw.len()) == Some(&b'\t'))
    })
}

/// Strip an "ability word — " prefix from a line.
/// Ability words are italicized flavor prefixes before an em dash, e.g.:
/// "Landfall — Whenever a land enters..." → "Whenever a land enters..."
/// "Spell mastery — If there are two or more..." → "If there are two or more..."
fn strip_ability_word(line: &str) -> Option<String> {
    split_short_label_prefix(line, 4).map(|(_, rest)| rest.to_string())
}

fn parse_oracle_block(lines: &[&str], start: usize) -> Option<(OracleBlockAst, usize)> {
    let line = strip_reminder_text(lines.get(start)?.trim());
    if line.is_empty() {
        return None;
    }

    let modes = collect_mode_asts(lines, start + 1);
    if modes.is_empty() {
        return None;
    }

    let next = start + 1 + modes.len();

    if let Some(colon_pos) = find_activated_colon(&line) {
        let cost_text = line[..colon_pos].trim();
        let effect_text = line[colon_pos + 1..].trim();
        if let Some(header) = parse_modal_header_ast(effect_text) {
            return Some((
                OracleBlockAst::ActivatedModal {
                    cost_text: cost_text.to_string(),
                    header,
                    modes,
                },
                next,
            ));
        }
    }

    let candidate = strip_ability_word(&line).unwrap_or_else(|| line.clone());
    let lower = candidate.to_lowercase();

    if let Some(header) = parse_modal_header_ast(&candidate) {
        if !lower.starts_with("when ")
            && !lower.starts_with("whenever ")
            && !lower.starts_with("at ")
        {
            return Some((OracleBlockAst::Modal { header, modes }, next));
        }
    }

    if let Some((trigger_line, header)) = split_triggered_modal_header(&candidate) {
        if let Some(header) = parse_modal_header_ast(&header) {
            return Some((
                OracleBlockAst::TriggeredModal {
                    trigger_line,
                    header,
                    modes,
                },
                next,
            ));
        }
    }

    // CR 702.172: Spree keyword line + all modes have per-mode costs
    if line.eq_ignore_ascii_case("spree")
        && !modes.is_empty()
        && modes.iter().all(|m| m.mode_cost.is_some())
    {
        let header = ModalHeaderAst {
            raw: line.to_string(),
            min_choices: 1,
            max_choices: modes.len(),
            allow_repeat_modes: false,
            constraints: vec![],
        };
        return Some((OracleBlockAst::Modal { header, modes }, next));
    }

    None
}

fn collect_mode_asts(lines: &[&str], start: usize) -> Vec<ModeAst> {
    let mut modes = Vec::new();

    for raw in lines.iter().skip(start) {
        let line = strip_reminder_text(raw.trim());
        if let Some(stripped) = line.strip_prefix('•') {
            modes.push(parse_mode_ast(stripped.trim()));
        } else if let Some(stripped) = line.strip_prefix('+') {
            // CR 702.172: Spree mode lines use `+ {cost} — effect` format
            let stripped = stripped.trim();
            if let Some((cost, rest)) = parse_mana_symbols(stripped) {
                // Strip " — " or " – " separator between cost and effect text
                let body = rest
                    .trim()
                    .strip_prefix('—')
                    .or_else(|| rest.trim().strip_prefix('–'))
                    .unwrap_or(rest)
                    .trim();
                modes.push(ModeAst {
                    raw: body.to_string(),
                    label: None,
                    body: body.to_string(),
                    mode_cost: Some(cost),
                });
            } else {
                break; // Cost parse failure → stop collecting modes
            }
        } else {
            break;
        }
    }

    modes
}

fn parse_mode_ast(text: &str) -> ModeAst {
    if let Some((label, body)) = split_short_label_prefix(text, 4) {
        return ModeAst {
            raw: text.to_string(),
            label: Some(label.to_string()),
            body: body.to_string(),
            mode_cost: None,
        };
    }

    ModeAst {
        raw: text.to_string(),
        label: None,
        body: text.to_string(),
        mode_cost: None,
    }
}

fn split_short_label_prefix(text: &str, max_words: usize) -> Option<(&str, &str)> {
    for sep in [" — ", " – ", " - "] {
        if let Some(pos) = text.find(sep) {
            let prefix = text[..pos].trim();
            let rest = text[pos + sep.len()..].trim();
            let word_count = prefix.split_whitespace().count();
            if (1..=max_words).contains(&word_count)
                && !prefix.contains('{')
                && !prefix.contains(':')
                && !rest.is_empty()
            {
                return Some((prefix, rest));
            }
        }
    }

    None
}

fn is_modal_header_text(lower: &str) -> bool {
    let lower = lower.trim();
    lower.starts_with("choose ")
        || lower.starts_with("you may choose ")
        || (lower.starts_with("if ") && lower.contains("choose "))
}

fn parse_modal_header_ast(text: &str) -> Option<ModalHeaderAst> {
    let sentences: Vec<&str> = text
        .split('.')
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .collect();
    let header_text = sentences.first().copied().unwrap_or(text).trim();
    let header_lower = header_text.to_lowercase();
    if !is_modal_header_text(&header_lower) {
        return None;
    }

    let (min_choices, max_choices) = parse_modal_choose_count(&text.to_lowercase());
    let mut allow_repeat_modes = false;
    let mut constraints = Vec::new();

    // CR 700.2: Detect cross-resolution mode restrictions from Oracle text.
    // The constraint phrase is part of the header sentence, not a period-delimited sub-sentence.
    // Order matters — "this turn" is the more specific substring.
    if header_lower.contains("that hasn't been chosen this turn") {
        constraints.push(ModalSelectionConstraint::NoRepeatThisTurn);
    } else if header_lower.contains("that hasn't been chosen") {
        constraints.push(ModalSelectionConstraint::NoRepeatThisGame);
    }

    for sentence in sentences.iter().skip(1) {
        let lower = sentence.to_lowercase();
        if lower == "you may choose the same mode more than once" {
            allow_repeat_modes = true;
            continue;
        }
        if lower == "each mode must target a different player" {
            constraints.push(ModalSelectionConstraint::DifferentTargetPlayers);
        }
    }

    Some(ModalHeaderAst {
        raw: text.to_string(),
        min_choices,
        max_choices,
        allow_repeat_modes,
        constraints,
    })
}

fn split_triggered_modal_header(line: &str) -> Option<(String, String)> {
    for (comma_pos, _) in line.match_indices(", ") {
        let trigger_line = line[..comma_pos].trim();
        let header = line[comma_pos + 2..].trim();
        if is_modal_header_text(&header.to_lowercase()) {
            return Some((trigger_line.to_string(), header.to_string()));
        }
    }

    None
}

fn lower_oracle_block(block: OracleBlockAst, card_name: &str, result: &mut ParsedAbilities) {
    match block {
        OracleBlockAst::ActivatedModal {
            cost_text,
            header,
            modes,
        } => {
            let def = build_modal_ability(AbilityKind::Activated, &header, &modes)
                .cost(parse_oracle_cost(&cost_text));
            result.abilities.push(def);
        }
        OracleBlockAst::Modal { header, modes } => {
            let modal = build_modal_choice(&header, &modes);
            let mode_abilities = lower_mode_abilities(&modes, AbilityKind::Spell);
            result.abilities.extend(mode_abilities);
            result.modal = Some(modal);
        }
        OracleBlockAst::TriggeredModal {
            trigger_line,
            header,
            modes,
        } => {
            let mut trigger = parse_trigger_line(&trigger_line, card_name);
            trigger.execute = Some(Box::new(build_modal_ability(
                AbilityKind::Spell,
                &header,
                &modes,
            )));
            result.triggers.push(trigger);
        }
    }
}

fn build_modal_ability(
    kind: AbilityKind,
    header: &ModalHeaderAst,
    modes: &[ModeAst],
) -> AbilityDefinition {
    AbilityDefinition::new(kind, modal_marker_effect(header)).with_modal(
        build_modal_choice(header, modes),
        lower_mode_abilities(modes, kind),
    )
}

fn modal_marker_effect(_header: &ModalHeaderAst) -> Effect {
    Effect::GenericEffect {
        static_abilities: vec![],
        duration: None,
        target: None,
    }
}

fn build_modal_choice(header: &ModalHeaderAst, modes: &[ModeAst]) -> ModalChoice {
    ModalChoice {
        min_choices: header.min_choices,
        max_choices: header.max_choices.min(modes.len()),
        mode_count: modes.len(),
        mode_descriptions: modes.iter().map(|mode| mode.raw.clone()).collect(),
        allow_repeat_modes: header.allow_repeat_modes,
        constraints: header.constraints.clone(),
        mode_costs: modes.iter().filter_map(|m| m.mode_cost.clone()).collect(),
    }
}

fn lower_mode_abilities(modes: &[ModeAst], kind: AbilityKind) -> Vec<AbilityDefinition> {
    modes
        .iter()
        .map(|mode| parse_effect_chain(&mode.body, kind))
        .collect()
}

/// Check if an AbilityDefinition (or its sub_ability chain) contains Unimplemented effects.
fn has_unimplemented(def: &AbilityDefinition) -> bool {
    if matches!(def.effect, Effect::Unimplemented { .. }) {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        return has_unimplemented(sub);
    }
    false
}

/// Check if a line looks like an effect sentence that `parse_effect` can normalize.
/// This mirrors the sentence-level shapes in the CubeArtisan grammar:
/// conditionals, subject + verb phrases, and bare imperatives.
fn is_effect_sentence_candidate(lower: &str) -> bool {
    let imperative_prefixes = [
        "add ",
        "attach ",
        "counter ",
        "create ",
        "deal ",
        "destroy ",
        "discard ",
        "draw ",
        "each player ",
        "each opponent ",
        "exile ",
        "explore",
        "fight ",
        "gain control ",
        "gain ",
        "look at ",
        "lose ",
        "mill ",
        "proliferate",
        "put ",
        "return ",
        "reveal ",
        "sacrifice ",
        "scry ",
        "search ",
        "shuffle ",
        "surveil ",
        "tap ",
        "untap ",
        "you may ",
    ];

    let subject_prefixes = [
        "all ", "if ", "it ", "target ", "that ", "they ", "this ", "those ", "you ",
    ];

    imperative_prefixes
        .iter()
        .chain(subject_prefixes.iter())
        .any(|prefix| lower.starts_with(prefix))
}

/// Parse the "choose N" count from the modal header line.
///
/// Returns (min_choices, max_choices). Examples:
/// - "choose one —" → (1, 1)
/// - "choose two —" → (2, 2)
/// - "choose one or both —" → (1, 2)
/// - "choose one or more —" → (1, usize::MAX) (capped to mode_count at construction)
/// - "choose any number of —" → (1, usize::MAX)
fn parse_modal_choose_count(lower: &str) -> (usize, usize) {
    let lower = lower.trim();
    let lower = lower.strip_prefix("you may ").unwrap_or(lower).trim_start();

    if lower.contains("choose any number instead") {
        return (1, usize::MAX);
    }
    if lower.contains("choose both instead") {
        return (1, 2);
    }
    if lower.contains("choose two instead") {
        return (1, 2);
    }
    if lower.contains("choose three instead") {
        return (1, 3);
    }
    if lower.contains("one or both") {
        return (1, 2);
    }
    if lower.contains("one or more") || lower.contains("any number") {
        return (1, usize::MAX);
    }
    // Extract the number word after "choose "
    if let Some(rest) = lower.strip_prefix("choose ") {
        if let Some((n, _)) = super::oracle_util::parse_number(rest) {
            return (n as usize, n as usize);
        }
    }
    // Default fallback
    (1, 1)
}

/// Create an Unimplemented fallback ability.
fn make_unimplemented(line: &str) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Unimplemented {
            name: "unknown".to_string(),
            description: Some(line.to_string()),
        },
    )
    .description(line.to_string())
}

/// Parse "As an additional cost to cast this spell, ..." into an `AdditionalCost`.
///
/// Recognized patterns:
/// - "you may blight N" → `Optional(Blight { count: N })`
/// - "blight N or pay {M}" → `Choice(Blight { count: N }, Mana { cost: M })`
/// - General "X or Y" → `Choice(X, Y)` using `parse_single_cost` for each fragment
fn parse_additional_cost_line(lower: &str, _raw: &str) -> Option<AdditionalCost> {
    // Pattern: "you may blight N" → Optional
    if let Some(pos) = lower.find("you may blight ") {
        let after = &lower[pos + "you may blight ".len()..];
        let count = parse_blight_count(after);
        return Some(AdditionalCost::Optional(AbilityCost::Blight { count }));
    }

    // Pattern: "blight N or pay {M}" → Choice (specific pattern with case-sensitive mana)
    if let Some(pos) = lower.find("blight ") {
        let after_blight = &lower[pos + "blight ".len()..];
        let count = parse_blight_count(after_blight);

        if let Some(or_pos) = after_blight.find(" or pay ") {
            let or_abs_pos = pos + "blight ".len() + or_pos + " or pay ".len();
            let mana_part = &_raw[or_abs_pos..];
            if let Some((mana_cost, _)) = parse_mana_symbols(mana_part.trim_end_matches('.')) {
                return Some(AdditionalCost::Choice(
                    AbilityCost::Blight { count },
                    AbilityCost::Mana { cost: mana_cost },
                ));
            }
        }
    }

    // General "X or Y" choice pattern using parse_single_cost for each fragment.
    // Strip the standard additional-cost prefix and trailing period.
    let body = lower
        .strip_prefix("as an additional cost to cast this spell, ")
        .unwrap_or(lower)
        .trim_end_matches('.');

    if let Some((left, right)) = body.split_once(" or ") {
        let cost_a = super::oracle_cost::parse_single_cost(left.trim());
        let cost_b = super::oracle_cost::parse_single_cost(right.trim());
        // Both fragments must parse to known costs — Unimplemented means the split was wrong
        // (e.g. "sacrifice an artifact or creature" splits incorrectly on " or ").
        if !matches!(cost_a, AbilityCost::Unimplemented { .. })
            && !matches!(cost_b, AbilityCost::Unimplemented { .. })
        {
            return Some(AdditionalCost::Choice(cost_a, cost_b));
        }
    }

    None
}

fn parse_spell_casting_option_line(text: &str, card_name: &str) -> Option<SpellCastingOption> {
    let trimmed = text.trim().trim_end_matches('.');
    let (condition, body) = split_leading_if_clause(trimmed);
    let primary_body = body.split_once(". ").map_or(body, |(head, _)| head).trim();
    let body_lower = primary_body.to_lowercase();

    parse_self_flash_option(primary_body, &body_lower, card_name)
        .or_else(|| parse_self_alternative_cost_option(primary_body, &body_lower, card_name))
        .map(|mut option| {
            if option.condition.is_none() {
                if let Some(condition) = condition {
                    option.condition = Some(condition.to_string());
                }
            }
            option
        })
}

fn split_leading_if_clause(text: &str) -> (Option<&str>, &str) {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    if !lower.starts_with("if ") {
        return (None, trimmed);
    }

    if let Some((condition, rest)) = trimmed.split_once(", ") {
        return (
            Some(condition.trim_start_matches("If ").trim()),
            rest.trim(),
        );
    }

    (None, trimmed)
}

fn parse_self_flash_option(
    body: &str,
    body_lower: &str,
    card_name: &str,
) -> Option<SpellCastingOption> {
    let self_ref = self_spell_phrase(body_lower, card_name)?;
    let prefix = format!("you may cast {self_ref} as though it had flash");
    if !body_lower.starts_with(&prefix) {
        return None;
    }

    let rest = body[prefix.len()..].trim();
    let mut option = SpellCastingOption::as_though_had_flash();

    if rest.is_empty() {
        return Some(option);
    }

    if let Some(cost_text) = rest
        .strip_prefix("if you pay ")
        .and_then(|rest| rest.strip_suffix(" more to cast it"))
    {
        option = option.cost(parse_oracle_cost(cost_text));
        return Some(option);
    }

    if let Some(cost_text) = rest
        .strip_prefix("by ")
        .and_then(|rest| rest.strip_suffix(" in addition to paying its other costs"))
    {
        option = option.cost(parse_oracle_cost(cost_text));
        return Some(option);
    }

    if let Some(condition) = rest.strip_prefix("if ") {
        option = option.condition(condition.trim());
        return Some(option);
    }

    Some(option)
}

fn parse_self_alternative_cost_option(
    body: &str,
    body_lower: &str,
    card_name: &str,
) -> Option<SpellCastingOption> {
    if let Some(cost_text) = extract_alternative_cost(
        body,
        body_lower,
        "you may pay ",
        " rather than pay this spell's mana cost",
    ) {
        return Some(SpellCastingOption::alternative_cost(parse_oracle_cost(
            cost_text,
        )));
    }

    if let Some((cost_text, condition)) = extract_alternative_cost_with_trailing_condition(
        body,
        body_lower,
        "you may pay ",
        " rather than pay this spell's mana cost if ",
    ) {
        return Some(
            SpellCastingOption::alternative_cost(parse_oracle_cost(cost_text)).condition(condition),
        );
    }

    if let Some(self_ref) = self_spell_phrase(body_lower, card_name) {
        let without_cost = format!("you may cast {self_ref} without paying its mana cost");
        if body_lower == without_cost {
            return Some(SpellCastingOption::free_cast());
        }

        let for_cost = format!("you may cast {self_ref} for ");
        if body_lower.starts_with(&for_cost) {
            let cost_text = body[for_cost.len()..].trim();
            return Some(SpellCastingOption::alternative_cost(parse_oracle_cost(
                cost_text,
            )));
        }
    }

    None
}

fn extract_alternative_cost<'a>(
    raw: &'a str,
    lower: &str,
    prefix: &str,
    suffix: &str,
) -> Option<&'a str> {
    if lower.starts_with(prefix) && lower.ends_with(suffix) {
        let cost_end = raw.len() - suffix.len();
        return Some(raw[prefix.len()..cost_end].trim());
    }

    None
}

fn extract_alternative_cost_with_trailing_condition<'a>(
    raw: &'a str,
    lower: &str,
    prefix: &str,
    marker: &str,
) -> Option<(&'a str, &'a str)> {
    if !lower.starts_with(prefix) {
        return None;
    }

    let marker_pos = lower.find(marker)?;
    let cost_text = raw[prefix.len()..marker_pos].trim();
    let condition = raw[marker_pos + marker.len()..].trim();
    Some((cost_text, condition))
}

fn self_spell_phrase(lower: &str, card_name: &str) -> Option<String> {
    let card_name_lower = card_name.to_lowercase();
    if lower.starts_with("you may cast this spell ") {
        return Some("this spell".to_string());
    }
    if lower.starts_with("you may cast it ") {
        return Some("it".to_string());
    }
    if lower.starts_with(&format!("you may cast {card_name_lower} ")) {
        return Some(card_name_lower);
    }

    None
}

fn parse_casting_restriction_line(text: &str) -> Option<Vec<CastingRestriction>> {
    let lower = text.trim().trim_end_matches('.').to_lowercase();
    let rest = lower.strip_prefix("cast this spell only ")?;
    let mut restrictions = Vec::new();

    if rest.contains("as a sorcery") {
        restrictions.push(CastingRestriction::AsSorcery);
    }
    if rest.contains("during combat") {
        restrictions.push(CastingRestriction::DuringCombat);
    }
    if rest.contains("during an opponent's turn")
        || rest.contains("during an opponents turn")
        || rest.contains("on an opponent's turn")
        || rest.contains("on an opponents turn")
    {
        restrictions.push(CastingRestriction::DuringOpponentsTurn);
    }
    if rest.contains("during your turn") {
        restrictions.push(CastingRestriction::DuringYourTurn);
    }
    if rest.contains("during your upkeep") {
        restrictions.push(CastingRestriction::DuringYourUpkeep);
    }
    if rest.contains("during any upkeep step") || rest.contains("during any upkeep") {
        restrictions.push(CastingRestriction::DuringAnyUpkeep);
    }
    if rest.contains("during an opponent's upkeep") || rest.contains("during an opponents upkeep") {
        restrictions.push(CastingRestriction::DuringOpponentsUpkeep);
    }
    if rest.contains("during your end step") {
        restrictions.push(CastingRestriction::DuringYourEndStep);
    }
    if rest.contains("during an opponent's end step")
        || rest.contains("during an opponents end step")
    {
        restrictions.push(CastingRestriction::DuringOpponentsEndStep);
    }
    if rest.contains("during the declare attackers step")
        || rest.contains("during your declare attackers step")
        || rest.contains("during declare attackers step")
    {
        restrictions.push(CastingRestriction::DeclareAttackersStep);
    }
    if rest.contains("during the declare blockers step")
        || rest.contains("during your declare blockers step")
        || rest.contains("during declare blockers step")
    {
        restrictions.push(CastingRestriction::DeclareBlockersStep);
    }
    if rest.contains("before attackers are declared") {
        restrictions.push(CastingRestriction::BeforeAttackersDeclared);
    }
    if rest.contains("before blockers are declared") {
        restrictions.push(CastingRestriction::BeforeBlockersDeclared);
    }
    if rest.contains("before the combat damage step") || rest.contains("before combat damage") {
        restrictions.push(CastingRestriction::BeforeCombatDamage);
    }
    if rest.contains("after combat") {
        restrictions.push(CastingRestriction::AfterCombat);
    }

    if let Some(condition) = rest.strip_prefix("if ") {
        restrictions.push(CastingRestriction::RequiresCondition {
            text: strip_casting_condition_suffixes(condition).to_string(),
        });
    }
    if let Some(condition) = rest.strip_prefix("only if ") {
        restrictions.push(CastingRestriction::RequiresCondition {
            text: strip_casting_condition_suffixes(condition).to_string(),
        });
    }
    if let Some(condition) = rest.split(" and only if ").nth(1) {
        restrictions.push(CastingRestriction::RequiresCondition {
            text: strip_casting_condition_suffixes(condition).to_string(),
        });
    }

    (!restrictions.is_empty()).then_some(restrictions)
}

fn strip_casting_condition_suffixes(text: &str) -> &str {
    text.trim()
        .trim_end_matches(" and only as a sorcery")
        .trim_end_matches(" and only during any upkeep step")
        .trim_end_matches(" and only during any upkeep")
        .trim()
}

/// Extract the blight count (N) from text starting after "blight ".
fn parse_blight_count(text: &str) -> u32 {
    text.split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1)
}

/// CR 719.1: Parse a Case's "To solve" condition text into a typed `SolveCondition`.
/// Handles "you control no {filter}" and falls back to `Text` for others.
fn parse_solve_condition(text: &str) -> SolveCondition {
    use crate::types::ability::{ControllerRef, FilterProp, TargetFilter};

    // "you control no suspected skeletons" → ObjectCount { filter, EQ, 0 }
    if let Some(rest) = text.strip_prefix("you control no ") {
        let rest = rest.trim_end_matches('.');
        let mut properties = Vec::new();

        // Check for "suspected" qualifier
        let rest = if let Some(after) = rest.strip_prefix("suspected ") {
            properties.push(FilterProp::Suspected);
            after
        } else {
            rest
        };

        // The remaining text is a subtype (e.g., "skeletons" → "Skeleton")
        let subtype = rest.trim().trim_end_matches('s');
        let subtype = super::oracle_effect::capitalize(subtype);

        let filter = TargetFilter::Typed(
            TypedFilter::creature()
                .subtype(subtype)
                .controller(ControllerRef::You)
                .properties(properties),
        );

        return SolveCondition::ObjectCount {
            filter,
            comparator: Comparator::EQ,
            threshold: 0,
        };
    }

    SolveCondition::Text {
        description: text.to_string(),
    }
}

/// Normalize self-references in a line for static ability parsing.
///
/// Replaces the card name (and legendary short name before comma) with `~`
/// so that `parse_static_line` can match patterns like "~ has".
fn normalize_self_refs_for_static(text: &str, card_name: &str) -> String {
    let mut result = text.to_string();
    // Replace full card name (case-insensitive)
    let lower_text = result.to_lowercase();
    let lower_name = card_name.to_lowercase();
    if let Some(pos) = lower_text.find(&lower_name) {
        result.replace_range(pos..pos + card_name.len(), "~");
    }
    // Legendary short name: "Kaito, Bane of Nightmares" → also match "Kaito"
    if let Some(comma_pos) = card_name.find(", ") {
        let short_name = &card_name[..comma_pos];
        let lower_result = result.to_lowercase();
        let lower_short = short_name.to_lowercase();
        if let Some(pos) = lower_result.find(&lower_short) {
            // Only replace if it wasn't already replaced by the full name
            if result[pos..].starts_with(short_name) || result[pos..].starts_with(&lower_short) {
                result.replace_range(pos..pos + short_name.len(), "~");
            }
        }
    }
    // Also replace "this creature", "this permanent", etc.
    for phrase in &[
        "this creature",
        "this land",
        "this permanent",
        "this enchantment",
    ] {
        result = result.replace(phrase, "~");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::QuantityExpr;
    use crate::types::mana::ManaCost;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    fn parse(
        text: &str,
        name: &str,
        kw: &[Keyword],
        types: &[&str],
        subtypes: &[&str],
    ) -> ParsedAbilities {
        let keyword_names: Vec<String> = kw.iter().map(keyword_display_name).collect();
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        parse_oracle_text(text, name, &keyword_names, &types, &subtypes)
    }

    /// Parse with raw MTGJSON keyword names (for testing keyword extraction).
    fn parse_with_keyword_names(
        text: &str,
        name: &str,
        keyword_names: &[&str],
        types: &[&str],
        subtypes: &[&str],
    ) -> ParsedAbilities {
        let keyword_names: Vec<String> = keyword_names.iter().map(|s| s.to_string()).collect();
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        parse_oracle_text(text, name, &keyword_names, &types, &subtypes)
    }

    #[test]
    fn lightning_bolt_spell_effect() {
        let r = parse(
            "Lightning Bolt deals 3 damage to any target.",
            "Lightning Bolt",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Spell);
    }

    #[test]
    fn llanowar_elves_mana_ability() {
        let r = parse(
            "{T}: Add {G}.",
            "Llanowar Elves",
            &[],
            &["Creature"],
            &["Elf", "Druid"],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
    }

    #[test]
    fn murder_spell_destroy() {
        let r = parse("Destroy target creature.", "Murder", &[], &["Instant"], &[]);
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Spell);
    }

    #[test]
    fn counterspell_spell_counter() {
        let r = parse(
            "Counter target spell.",
            "Counterspell",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
    }

    #[test]
    fn bonesplitter_static_plus_equip() {
        let r = parse(
            "Equipped creature gets +2/+0.\nEquip {1}",
            "Bonesplitter",
            &[],
            &["Artifact"],
            &["Equipment"],
        );
        assert_eq!(r.statics.len(), 1);
        assert_eq!(r.abilities.len(), 1); // equip ability
    }

    #[test]
    fn rancor_enchant_static_trigger() {
        let r = parse(
            "Enchant creature\nEnchanted creature gets +2/+0 and has trample.\nWhen Rancor is put into a graveyard from the battlefield, return Rancor to its owner's hand.",
            "Rancor",
            &[],
            &["Enchantment"],
            &["Aura"],
        );
        // Enchant line skipped (priority 2)
        assert_eq!(r.statics.len(), 1);
        assert_eq!(r.triggers.len(), 1);
    }

    #[test]
    fn non_spell_target_sentence_routes_to_effect_parser() {
        let r = parse(
            "Target player draws a card.",
            "Test Permanent",
            &[],
            &["Artifact"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(matches!(
            r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
    }

    #[test]
    fn non_spell_conditional_sentence_routes_to_effect_parser() {
        let r = parse(
            "If you sacrificed a Food this turn, draw a card.",
            "Test Permanent",
            &[],
            &["Enchantment"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(matches!(
            r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
    }

    #[test]
    fn player_shroud_routes_to_static_parser() {
        let r = parse("You have shroud.", "Ivory Mask", &[], &["Enchantment"], &[]);
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::Other("Shroud".to_string())
        );
    }

    #[test]
    fn top_of_library_peek_routes_to_static_parser() {
        let r = parse(
            "You may look at the top card of your library any time.",
            "Bolas's Citadel",
            &[],
            &["Artifact"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::Other("MayLookAtTopOfLibrary".to_string())
        );
    }

    #[test]
    fn lose_all_abilities_routes_to_static_parser() {
        let r = parse(
            "Cards in graveyards lose all abilities.",
            "Yixlid Jailer",
            &[],
            &["Creature"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert!(r.statics[0]
            .modifications
            .contains(&crate::types::ability::ContinuousModification::RemoveAllAbilities));
    }

    #[test]
    fn colored_creature_lord_routes_to_static_parser() {
        let r = parse(
            "Black creatures get +1/+1.",
            "Bad Moon",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert!(r.statics[0]
            .modifications
            .contains(&crate::types::ability::ContinuousModification::AddPower { value: 1 }));
    }

    #[test]
    fn filtered_creatures_you_control_route_to_static_parser() {
        let r = parse(
            "Creatures you control with mana value 3 or less get +1/+0.",
            "Hero of the Dunes",
            &[],
            &["Creature"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert!(matches!(
            r.statics[0].affected,
            Some(crate::types::ability::TargetFilter::Typed(
                crate::types::ability::TypedFilter {
                    controller: Some(crate::types::ability::ControllerRef::You),
                    ..
                }
            ))
        ));
    }

    #[test]
    fn favorable_winds_routes_to_static_parser() {
        let r = parse(
            "Creatures you control with flying get +1/+1.",
            "Favorable Winds",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert!(matches!(
            r.statics[0].affected,
            Some(crate::types::ability::TargetFilter::Typed(
                crate::types::ability::TypedFilter {
                    controller: Some(crate::types::ability::ControllerRef::You),
                    ref properties,
                    ..
                }
            )) if properties == &vec![crate::types::ability::FilterProp::WithKeyword {
                value: "flying".to_string(),
            }]
        ));
    }

    #[test]
    fn must_attack_routes_to_static_parser() {
        let r = parse(
            "This creature attacks each combat if able.",
            "Primordial Ooze",
            &[],
            &["Creature"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::MustAttack
        );
    }

    #[test]
    fn no_maximum_hand_size_routes_to_static_parser() {
        let r = parse(
            "You have no maximum hand size.",
            "Spellbook",
            &[],
            &["Artifact"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::Other("NoMaximumHandSize".to_string())
        );
    }

    #[test]
    fn block_restriction_routes_to_static_parser() {
        let r = parse(
            "This creature can block only creatures with flying.",
            "Cloud Pirates",
            &[],
            &["Creature"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::Other("BlockRestriction".to_string())
        );
    }

    #[test]
    fn granted_activated_static_routes_before_colon_parse() {
        let r = parse(
            "Enchanted land has \"{T}: Add two mana of any one color.\"",
            "Gift of Paradise",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
        let grant = r.statics[0].modifications.iter().find(|m| {
            matches!(
                m,
                crate::types::ability::ContinuousModification::GrantAbility { .. }
            )
        });
        assert!(
            grant.is_some(),
            "should contain a GrantAbility modification"
        );
        if let crate::types::ability::ContinuousModification::GrantAbility { definition } =
            grant.unwrap()
        {
            assert_eq!(
                definition.kind,
                crate::types::ability::AbilityKind::Activated
            );
        }
    }

    #[test]
    fn quoted_granted_ability_is_not_misclassified_as_activated() {
        let r = parse(
            "White creatures you control have \"{T}: You gain 1 life.\"",
            "Resplendent Mentor",
            &[],
            &["Creature"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.statics.len(), 1);
    }

    #[test]
    fn activated_as_sorcery_constraint_sets_sorcery_speed() {
        let r = parse(
            "{2}{W}, Sacrifice this artifact: Target creature you control gets +2/+2 and gains flying until end of turn. Draw a card. Activate only as a sorcery.",
            "Basilica Skullbomb",
            &[],
            &["Artifact"],
            &[],
        );

        assert_eq!(r.abilities.len(), 1);
        assert!(r.abilities[0].sorcery_speed);
        assert!(r.abilities[0]
            .activation_restrictions
            .contains(&crate::types::ability::ActivationRestriction::AsSorcery));
        let draw = r.abilities[0]
            .sub_ability
            .as_ref()
            .expect("expected draw follow-up");
        assert!(matches!(
            draw.effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
        let no_activate_tail = draw
            .sub_ability
            .as_ref()
            .is_none_or(|tail| !matches!(tail.effect, Effect::Unimplemented { ref name, .. } if name == "activate"));
        assert!(no_activate_tail);
    }

    #[test]
    fn spell_cast_restrictions_parse_into_top_level_metadata() {
        let r = parse(
            "Cast this spell only during combat on an opponent's turn.\nReturn X target creature cards from your graveyard to the battlefield. Sacrifice those creatures at the beginning of the next end step.",
            "Wake the Dead",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(
            r.casting_restrictions,
            vec![
                CastingRestriction::DuringCombat,
                CastingRestriction::DuringOpponentsTurn,
            ]
        );
        assert!(!matches!(
            r.abilities[0].effect,
            Effect::Unimplemented { ref name, .. } if name == "cast"
        ));
    }

    #[test]
    fn spell_cast_restriction_condition_is_preserved() {
        let restrictions = parse_casting_restriction_line(
            "Cast this spell only during the declare attackers step and only if you've been attacked this step.",
        )
        .expect("restrictions should parse");
        assert_eq!(
            restrictions,
            vec![
                CastingRestriction::DeclareAttackersStep,
                CastingRestriction::RequiresCondition {
                    text: "you've been attacked this step".to_string(),
                },
            ]
        );
    }

    #[test]
    fn spell_cast_restriction_parses_end_step_window() {
        let restrictions =
            parse_casting_restriction_line("Cast this spell only during your end step.")
                .expect("restrictions should parse");
        assert_eq!(restrictions, vec![CastingRestriction::DuringYourEndStep]);
    }

    #[test]
    fn spell_cast_restriction_parses_opponent_upkeep_window() {
        let restrictions =
            parse_casting_restriction_line("Cast this spell only during an opponent's upkeep.")
                .expect("restrictions should parse");
        assert_eq!(
            restrictions,
            vec![CastingRestriction::DuringOpponentsUpkeep]
        );
    }

    #[test]
    fn spell_cast_restriction_parses_any_upkeep_window() {
        let restrictions =
            parse_casting_restriction_line("Cast this spell only during any upkeep step.")
                .expect("restrictions should parse");
        assert_eq!(restrictions, vec![CastingRestriction::DuringAnyUpkeep]);
    }

    #[test]
    fn spell_cast_restriction_parses_plain_only_if_condition() {
        let restrictions = parse_casting_restriction_line(
            "Cast this spell only if you control two or more Vampires.",
        )
        .expect("restrictions should parse");
        assert_eq!(
            restrictions,
            vec![CastingRestriction::RequiresCondition {
                text: "you control two or more vampires".to_string(),
            }]
        );
    }

    #[test]
    fn spell_cast_restriction_splits_as_sorcery_from_condition() {
        let restrictions = parse_casting_restriction_line(
            "Cast this spell only if there are four or more card types among cards in your graveyard and only as a sorcery.",
        )
        .expect("restrictions should parse");
        assert_eq!(
            restrictions,
            vec![
                CastingRestriction::AsSorcery,
                CastingRestriction::RequiresCondition {
                    text: "there are four or more card types among cards in your graveyard"
                        .to_string(),
                },
            ]
        );
    }

    #[test]
    fn spell_cast_restriction_parses_your_declare_attackers_step_variant() {
        let restrictions = parse_casting_restriction_line(
            "Cast this spell only during your declare attackers step.",
        )
        .expect("restrictions should parse");
        assert_eq!(restrictions, vec![CastingRestriction::DeclareAttackersStep]);
    }

    #[test]
    fn spell_casting_option_parses_trap_alternative_cost() {
        let r = parse(
            "If an opponent searched their library this turn, you may pay {0} rather than pay this spell's mana cost.\nTarget opponent mills thirteen cards.",
            "Archive Trap",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.casting_options.len(), 1);
        assert_eq!(
            r.casting_options[0],
            SpellCastingOption::alternative_cost(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 0,
                    shards: vec![],
                },
            })
            .condition("an opponent searched their library this turn")
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(!matches!(
            r.abilities[0].effect,
            Effect::Unimplemented { ref name, .. } if name == "pay"
        ));
    }

    #[test]
    fn spell_casting_option_parses_composite_alternative_cost() {
        let r = parse(
            "You may pay 1 life and exile a blue card from your hand rather than pay this spell's mana cost.\nCounter target spell.",
            "Force of Will",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.casting_options.len(), 1);
        assert!(matches!(
            r.casting_options[0].cost,
            Some(AbilityCost::Composite { .. })
        ));
    }

    #[test]
    fn spell_casting_option_parses_flash_permission_with_extra_cost() {
        let r = parse(
            "You may cast this spell as though it had flash if you pay {2} more to cast it.\nDestroy all creatures. They can't be regenerated.",
            "Rout",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.casting_options.len(), 1);
        assert_eq!(
            r.casting_options[0],
            SpellCastingOption::as_though_had_flash().cost(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 2,
                    shards: vec![],
                },
            })
        );
        assert_eq!(r.abilities.len(), 1);
    }

    #[test]
    fn spell_casting_option_parses_free_cast_condition() {
        let r = parse(
            "If this spell is the first spell you've cast this game, you may cast it without paying its mana cost.\nLook at the top five cards of your library.",
            "Once Upon a Time",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(
            r.casting_options,
            vec![SpellCastingOption::free_cast()
                .condition("this spell is the first spell you've cast this game")]
        );
    }

    #[test]
    fn spell_casting_option_ignores_followup_if_you_do_sentence() {
        let r = parse(
            "Return up to two target creature cards from your graveyard to your hand.\nYou may cast this spell for {2}{B/G}{B/G}. If you do, ignore the bracketed text.",
            "Graveyard Dig",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(
            r.casting_options,
            vec![SpellCastingOption::alternative_cost(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 2,
                    shards: vec![
                        crate::types::mana::ManaCostShard::BlackGreen,
                        crate::types::mana::ManaCostShard::BlackGreen,
                    ],
                },
            })]
        );
    }

    #[test]
    fn goblin_chainwhirler_etb_trigger() {
        let r = parse(
            "First strike\nWhen Goblin Chainwhirler enters the battlefield, it deals 1 damage to each opponent and each creature and planeswalker they control.",
            "Goblin Chainwhirler",
            &[Keyword::FirstStrike],
            &["Creature"],
            &["Goblin", "Warrior"],
        );
        assert_eq!(r.triggers.len(), 1);
        assert_eq!(r.abilities.len(), 0); // keyword line skipped
    }

    #[test]
    fn baneslayer_angel_keywords_only() {
        let r = parse(
            "Flying, first strike, lifelink, protection from Demons and from Dragons",
            "Baneslayer Angel",
            &[Keyword::Flying, Keyword::FirstStrike, Keyword::Lifelink],
            &["Creature"],
            &["Angel"],
        );
        // Keywords line should be mostly skipped; protection clause may produce unimplemented
        // The key assertion: no activated abilities, no triggers
        assert_eq!(r.abilities.len(), 0);
        assert_eq!(r.triggers.len(), 0);
    }

    #[test]
    fn questing_beast_mixed() {
        let r = parse(
            "Vigilance, deathtouch, haste\nQuesting Beast can't be blocked by creatures with power 2 or less.\nCombat damage that would be dealt by creatures you control can't be prevented.\nWhenever Questing Beast deals combat damage to a planeswalker, it deals that much damage to target planeswalker that player controls.",
            "Questing Beast",
            &[Keyword::Vigilance, Keyword::Deathtouch, Keyword::Haste],
            &["Creature"],
            &["Beast"],
        );
        // "can't be prevented" now parses as an ability (Effect::AddRestriction) rather than replacement
        assert_eq!(r.abilities.len(), 1);
        assert!(matches!(
            r.abilities[0].effect,
            Effect::AddRestriction { .. }
        ));
        // Should have static and trigger
        assert!(!r.statics.is_empty());
        assert!(!r.triggers.is_empty());
    }

    #[test]
    fn jace_loyalty_abilities() {
        let r = parse(
            "+2: Look at the top card of target player's library. You may put that card on the bottom of that player's library.\n0: Draw three cards, then put two cards from your hand on top of your library in any order.\n\u{2212}1: Return target creature to its owner's hand.\n\u{2212}12: Exile all cards from target player's library, then that player shuffles their hand into their library.",
            "Jace, the Mind Sculptor",
            &[],
            &["Planeswalker"],
            &["Jace"],
        );
        assert_eq!(r.abilities.len(), 4);
        // All should be activated with loyalty costs
        for ab in &r.abilities {
            assert_eq!(ab.kind, AbilityKind::Activated);
        }
    }

    #[test]
    fn forest_reminder_text_only() {
        let r = parse("({T}: Add {G}.)", "Forest", &[], &["Land"], &["Forest"]);
        // Reminder text should be stripped/skipped
        assert_eq!(r.abilities.len(), 0);
    }

    #[test]
    fn mox_pearl_mana_ability() {
        let r = parse("{T}: Add {W}.", "Mox Pearl", &[], &["Artifact"], &[]);
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
    }

    #[test]
    fn parses_activate_only_land_condition_into_activation_restriction() {
        let r = parse(
            "{T}: Add {U}.\n{T}: Add {B}. Activate only if you control an Island or a Swamp.",
            "Gloomlake Verge",
            &[],
            &["Land"],
            &[],
        );
        assert_eq!(r.abilities.len(), 2);
        let second = &r.abilities[1];
        assert!(matches!(
            second.activation_restrictions.as_slice(),
            [ActivationRestriction::RequiresCondition { text }]
                if text == "you control an Island or a Swamp"
        ));
    }

    #[test]
    fn parses_compound_activate_only_constraints() {
        let r = parse(
            "{T}: Add {R}. Activate only as a sorcery and only once each turn.",
            "Careful Forge",
            &[],
            &["Artifact"],
            &[],
        );
        assert_eq!(
            r.abilities[0].activation_restrictions,
            vec![
                ActivationRestriction::AsSorcery,
                ActivationRestriction::OnlyOnceEachTurn,
            ]
        );
    }

    #[test]
    fn extracts_protection_keyword_from_oracle_text() {
        use crate::types::keywords::ProtectionTarget;
        // Soldier of the Pantheon: MTGJSON lists "Protection" as keyword name,
        // Oracle text has the full "Protection from multicolored"
        let r = parse_with_keyword_names(
            "Protection from multicolored",
            "Soldier of the Pantheon",
            &["protection"], // MTGJSON keyword name (lowercased)
            &["Creature"],
            &["Human", "Soldier"],
        );
        assert_eq!(r.extracted_keywords.len(), 1);
        assert!(matches!(
            &r.extracted_keywords[0],
            Keyword::Protection(ProtectionTarget::Multicolored)
        ));
    }

    #[test]
    fn skips_keywords_already_in_mtgjson() {
        // "Flying" is in MTGJSON — exact name match, should not be re-extracted
        let r = parse_with_keyword_names(
            "Flying",
            "Serra Angel",
            &["flying", "vigilance"],
            &["Creature"],
            &["Angel"],
        );
        assert!(r.extracted_keywords.is_empty());
    }

    #[test]
    fn extracts_new_keywords_from_mixed_line() {
        use crate::types::keywords::ProtectionTarget;
        // "flying" exact-matches MTGJSON (skipped), "protection from red" prefix-matches (extracted)
        let r = parse_with_keyword_names(
            "Flying, protection from red",
            "Test Card",
            &["flying", "protection"],
            &["Creature"],
            &[],
        );
        assert_eq!(r.extracted_keywords.len(), 1);
        assert!(matches!(
            &r.extracted_keywords[0],
            Keyword::Protection(ProtectionTarget::Color(crate::types::mana::ManaColor::Red))
        ));
    }

    #[test]
    fn no_extraction_without_mtgjson_keywords() {
        // Without MTGJSON keywords, keyword-only lines are not detected
        // (prevents false positives like "Equip {1}" being eaten)
        let r = parse_with_keyword_names(
            "Equip {1}",
            "Bonesplitter",
            &[],
            &["Artifact"],
            &["Equipment"],
        );
        assert!(r.extracted_keywords.is_empty());
        // Line should fall through to equip ability parsing
        assert_eq!(r.abilities.len(), 1);
    }

    // ── Modal parsing tests ──────────────────────────────────────────────

    #[test]
    fn choose_one_modal_metadata() {
        let r = parse(
            "Choose one —\n• Deal 3 damage to any target.\n• Draw a card.\n• Gain 3 life.",
            "Test Charm",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 3);
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 3);
        assert_eq!(modal.mode_descriptions.len(), 3);
    }

    #[test]
    fn choose_two_modal_metadata() {
        let r = parse(
            "Choose two —\n• Counter target spell.\n• Return target permanent to its owner's hand.\n• Tap all creatures your opponents control.\n• Draw a card.",
            "Cryptic Command",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 4);
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 2);
        assert_eq!(modal.max_choices, 2);
        assert_eq!(modal.mode_count, 4);
    }

    #[test]
    fn choose_one_or_both_modal_metadata() {
        let r = parse(
            "Choose one or both —\n• Destroy target artifact.\n• Destroy target enchantment.",
            "Wear // Tear",
            &[],
            &["Instant"],
            &[],
        );
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 2);
        assert_eq!(modal.mode_count, 2);
    }

    #[test]
    fn choose_one_conditional_choose_both_modal_metadata() {
        let r = parse(
            "Choose one. If you control a commander as you cast this spell, you may choose both instead.\n• Draw a card.\n• Gain 3 life.",
            "Will Test",
            &[],
            &["Instant"],
            &[],
        );
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 2);
        assert_eq!(modal.mode_count, 2);
        assert!(matches!(
            r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
        assert!(matches!(
            r.abilities[1].effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
    }

    #[test]
    fn ability_word_modal_block_strips_prefix_before_modal_parse() {
        let r = parse(
            "Delirium — Choose one. If there are four or more card types among cards in your graveyard, choose both instead.\n• Draw a card.\n• Gain 3 life.",
            "Test Delirium",
            &[],
            &["Instant"],
            &[],
        );
        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 2);
        assert_eq!(modal.mode_count, 2);
        assert!(matches!(
            r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
        assert!(matches!(
            r.abilities[1].effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
    }

    #[test]
    fn labeled_modal_bullets_use_effect_bodies() {
        let r = parse(
            "Choose one —\n• Alpha — Draw a card.\n• Beta — Gain 3 life.",
            "Test Charm",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 2);
        assert!(matches!(
            r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
        assert!(matches!(
            r.abilities[1].effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));

        let modal = r.modal.expect("should have modal metadata");
        assert_eq!(
            modal.mode_descriptions,
            vec![
                "Alpha — Draw a card.".to_string(),
                "Beta — Gain 3 life.".to_string()
            ]
        );
    }

    #[test]
    fn triggered_modal_block_routes_modes_through_effect_parser() {
        let r = parse(
            "When you set this scheme in motion, choose one —\n• Search your library for a creature card, reveal it, put it into your hand, then shuffle.\n• You may put a creature card from your hand onto the battlefield.",
            "Introductions Are In Order",
            &[],
            &["Scheme"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.triggers.len(), 1);

        let trigger = &r.triggers[0];
        assert_eq!(trigger.mode, TriggerMode::SetInMotion);

        let execute = trigger
            .execute
            .as_ref()
            .expect("trigger should have execute");
        assert!(matches!(
            execute.effect,
            Effect::GenericEffect {
                ref static_abilities,
                duration: None,
                target: None,
            } if static_abilities.is_empty()
        ));
        let modal = execute.modal.as_ref().expect("execute should be modal");
        assert_eq!(modal.mode_count, 2);
        assert_eq!(execute.mode_abilities.len(), 2);

        assert!(matches!(
            execute.mode_abilities[0].effect,
            Effect::SearchLibrary { .. }
        ));
        let search_sub = execute.mode_abilities[0]
            .sub_ability
            .as_ref()
            .expect("search mode should have change-zone followup");
        assert!(matches!(
            search_sub.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                ..
            }
        ));

        assert!(matches!(
            execute.mode_abilities[1].effect,
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                ..
            }
        ));
    }

    #[test]
    fn triggered_modal_labeled_modes_strip_labels_before_effect_parse() {
        let r = parse(
            "At the beginning of your upkeep, choose one that hasn't been chosen —\n• Buffet — Create three Food tokens.\n• See a Show — Create two 2/2 white Performer creature tokens.\n• Play Games — Search your library for a card, put that card into your hand, discard a card at random, then shuffle.\n• Go to Sleep — You lose 15 life. Sacrifice Night Out in Vegas.",
            "Night Out in Vegas",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.triggers.len(), 1);

        let execute = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        let modal = execute.modal.as_ref().expect("execute should be modal");
        assert_eq!(modal.mode_count, 4);
        assert_eq!(
            modal.constraints,
            vec![ModalSelectionConstraint::NoRepeatThisGame]
        );
        assert_eq!(execute.mode_abilities.len(), 4);

        assert!(matches!(
            execute.mode_abilities[2].effect,
            Effect::SearchLibrary { .. }
        ));
        let search_sub = execute.mode_abilities[2]
            .sub_ability
            .as_ref()
            .expect("play games mode should have change-zone followup");
        assert!(matches!(
            search_sub.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                ..
            }
        ));

        assert!(matches!(
            execute.mode_abilities[3].effect,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 15 }
            }
        ));
    }

    #[test]
    fn triggered_modal_header_supports_you_may_choose_and_constraints() {
        let r = parse(
            "At the beginning of combat on your turn, you may choose two. Each mode must target a different player.\n• Target player creates a 2/1 white and black Inkling creature token with flying.\n• Target player draws a card and loses 1 life.\n• Target player puts a +1/+1 counter on each creature they control.",
            "Shadrix Silverquill",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        let execute = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        let modal = execute.modal.as_ref().expect("execute should be modal");
        assert_eq!(modal.min_choices, 2);
        assert_eq!(modal.max_choices, 2);
        assert_eq!(modal.mode_count, 3);
        assert_eq!(
            modal.constraints,
            vec![ModalSelectionConstraint::DifferentTargetPlayers]
        );
    }

    #[test]
    fn modal_header_tracks_repeatable_modes() {
        let header = parse_modal_header_ast(
            "Choose up to five {P} worth of modes. You may choose the same mode more than once.",
        )
        .expect("header should parse");
        assert!(header.allow_repeat_modes);
    }

    #[test]
    fn modal_header_detects_no_repeat_this_turn_constraint() {
        let header = parse_modal_header_ast("choose one that hasn't been chosen this turn —")
            .expect("header should parse");
        assert_eq!(
            header.constraints,
            vec![ModalSelectionConstraint::NoRepeatThisTurn]
        );
    }

    #[test]
    fn modal_header_detects_no_repeat_this_game_constraint() {
        let header = parse_modal_header_ast("choose one that hasn't been chosen —")
            .expect("header should parse");
        assert_eq!(
            header.constraints,
            vec![ModalSelectionConstraint::NoRepeatThisGame]
        );
    }

    #[test]
    fn monument_to_endurance_parses_no_repeat_this_turn() {
        let r = parse(
            "At the beginning of your end step, choose one that hasn't been chosen this turn —\n• Put a +1/+1 counter on Monument to Endurance.\n• You gain 4 life.\n• Create a 0/0 green Hydra creature token with \"This creature gets +1/+1 for each counter on it.\"",
            "Monument to Endurance",
            &[],
            &["Enchantment", "Creature"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        let execute = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        let modal = execute.modal.as_ref().expect("execute should be modal");
        assert_eq!(modal.mode_count, 3);
        assert_eq!(
            modal.constraints,
            vec![ModalSelectionConstraint::NoRepeatThisTurn]
        );
        assert_eq!(execute.mode_abilities.len(), 3);
    }

    #[test]
    fn non_modal_spell_has_no_modal_metadata() {
        let r = parse(
            "Deal 3 damage to any target.",
            "Lightning Bolt",
            &[],
            &["Instant"],
            &[],
        );
        assert!(r.modal.is_none());
    }

    #[test]
    fn parse_modal_choose_count_variants() {
        assert_eq!(parse_modal_choose_count("choose one —"), (1, 1));
        assert_eq!(parse_modal_choose_count("choose two —"), (2, 2));
        assert_eq!(parse_modal_choose_count("you may choose two."), (2, 2));
        assert_eq!(parse_modal_choose_count("choose three —"), (3, 3));
        assert_eq!(parse_modal_choose_count("choose one or both —"), (1, 2));
        assert_eq!(
            parse_modal_choose_count("choose one or more —"),
            (1, usize::MAX)
        );
        assert_eq!(
            parse_modal_choose_count("choose any number of —"),
            (1, usize::MAX)
        );
    }

    #[test]
    fn modal_activated_ability_bow_of_nylea() {
        let r = parse(
            "Attacking creatures you control have deathtouch.\n{1}{G}, {T}: Choose one —\n• Put a +1/+1 counter on target creature.\n• Bow of Nylea deals 2 damage to target creature with flying.\n• You gain 3 life.\n• Put up to four target cards from your graveyard on the bottom of your library in any order.",
            "Bow of Nylea",
            &[],
            &["Enchantment", "Artifact"],
            &[],
        );
        // First ability is the static deathtouch line, parsed as a regular ability
        // Second ability is the modal activated ability
        let modal_def = r.abilities.iter().find(|a| a.modal.is_some());
        assert!(modal_def.is_some(), "should have a modal activated ability");
        let modal_def = modal_def.unwrap();
        let modal = modal_def.modal.as_ref().unwrap();
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 4);
        assert_eq!(modal_def.mode_abilities.len(), 4);
        assert!(modal_def.cost.is_some(), "should have a cost");
    }

    #[test]
    fn modal_activated_ability_cankerbloom() {
        let r = parse(
            "{1}, Sacrifice Cankerbloom: Choose one —\n• Destroy target artifact.\n• Destroy target enchantment.",
            "Cankerbloom",
            &[],
            &["Creature"],
            &[],
        );
        let modal_def = r.abilities.iter().find(|a| a.modal.is_some());
        assert!(modal_def.is_some(), "should have a modal activated ability");
        let modal = modal_def.unwrap().modal.as_ref().unwrap();
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 1);
        assert_eq!(modal.mode_count, 2);
        // Spell-level modal should NOT be set (this is an activated ability modal)
        assert!(r.modal.is_none(), "spell-level modal should be None");
    }

    #[test]
    fn modal_activated_ability_uses_normalized_mode_bodies() {
        let r = parse(
            "{1}, {T}: Choose one —\n• Alpha — Draw a card.\n• Beta — Gain 3 life.",
            "Test Relic",
            &[],
            &["Artifact"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let modal_def = &r.abilities[0];
        let modal = modal_def
            .modal
            .as_ref()
            .expect("should have modal metadata");
        assert_eq!(modal.mode_count, 2);
        assert_eq!(modal_def.mode_abilities.len(), 2);
        assert!(matches!(
            modal_def.mode_abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
        assert!(matches!(
            modal_def.mode_abilities[1].effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
        assert!(modal_def.cost.is_some(), "should preserve activated cost");
    }

    #[test]
    fn parse_additional_cost_optional_blight() {
        let lower = "as an additional cost to cast this spell, you may blight 1.";
        let raw = "As an additional cost to cast this spell, you may blight 1.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Optional(AbilityCost::Blight { count: 1 }))
        );
    }

    #[test]
    fn parse_additional_cost_optional_blight_2() {
        let lower = "as an additional cost to cast this spell, you may blight 2.";
        let raw = "As an additional cost to cast this spell, you may blight 2.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Optional(AbilityCost::Blight { count: 2 }))
        );
    }

    #[test]
    fn parse_additional_cost_choice_blight_or_pay() {
        let lower = "as an additional cost to cast this spell, blight 2 or pay {1}.";
        let raw = "As an additional cost to cast this spell, blight 2 or pay {1}.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Choice(
                AbilityCost::Blight { count: 2 },
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        generic: 1,
                        shards: vec![]
                    }
                }
            ))
        );
    }

    #[test]
    fn parse_additional_cost_choice_blight_or_pay_3() {
        let lower = "as an additional cost to cast this spell, blight 1 or pay {3}.";
        let raw = "As an additional cost to cast this spell, blight 1 or pay {3}.";
        let result = parse_additional_cost_line(lower, raw);
        assert_eq!(
            result,
            Some(AdditionalCost::Choice(
                AbilityCost::Blight { count: 1 },
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        generic: 3,
                        shards: vec![]
                    }
                }
            ))
        );
    }

    #[test]
    fn parse_additional_cost_mandatory_blight_skipped() {
        // Mandatory blight (no "you may", no "or") — not yet modeled
        let lower = "as an additional cost to cast this spell, blight 2.";
        let raw = "As an additional cost to cast this spell, blight 2.";
        let result = parse_additional_cost_line(lower, raw);
        // Mandatory without "or" currently falls through (no choice to present)
        assert!(result.is_none());
    }

    #[test]
    fn parse_additional_cost_discard_or_pay_life() {
        let lower = "as an additional cost to cast this spell, discard a card or pay 3 life.";
        let raw = "As an additional cost to cast this spell, discard a card or pay 3 life.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Choice(
                AbilityCost::Discard {
                    count: 1,
                    random: false,
                    ..
                },
                AbilityCost::PayLife { amount: 3 },
            )) => {}
            other => panic!("Expected Choice(Discard, PayLife), got {:?}", other),
        }
    }

    #[test]
    fn parse_additional_cost_sacrifice_or_mana() {
        let lower = "as an additional cost to cast this spell, sacrifice a creature or pay {2}.";
        let raw = "As an additional cost to cast this spell, sacrifice a creature or pay {2}.";
        let result = parse_additional_cost_line(lower, raw);
        match result {
            Some(AdditionalCost::Choice(
                AbilityCost::Sacrifice { .. },
                AbilityCost::Mana { .. },
            )) => {}
            other => panic!("Expected Choice(Sacrifice, Mana), got {:?}", other),
        }
    }

    #[test]
    fn parse_additional_cost_sacrifice_compound_type_not_choice() {
        // "sacrifice an artifact or creature" is a single sacrifice cost, not a choice
        let lower = "as an additional cost to cast this spell, sacrifice an artifact or creature.";
        let raw = "As an additional cost to cast this spell, sacrifice an artifact or creature.";
        let result = parse_additional_cost_line(lower, raw);
        // Should return None — "creature" alone is Unimplemented, rejecting the split
        assert!(result.is_none());
    }

    // ── Spree (CR 702.172) ──────────────────────────────────────────────

    #[test]
    fn spree_phantom_interference_parses_modal_with_mode_costs() {
        let text = "Spree (Choose one or more additional costs.)\n\
                     + {3} — Create a 2/2 white Spirit creature token with flying.\n\
                     + {1} — Counter target spell unless its controller pays {2}.";
        let result = parse(
            text,
            "Phantom Interference",
            &[Keyword::Spree],
            &["Instant"],
            &[],
        );
        let modal = result.modal.expect("should have modal");
        assert_eq!(modal.min_choices, 1);
        assert_eq!(modal.max_choices, 2);
        assert_eq!(modal.mode_count, 2);
        assert_eq!(modal.mode_costs.len(), 2);
        // Mode 0: {3}
        assert_eq!(
            modal.mode_costs[0],
            ManaCost::Cost {
                shards: vec![],
                generic: 3
            }
        );
        // Mode 1: {1}
        assert_eq!(
            modal.mode_costs[1],
            ManaCost::Cost {
                shards: vec![],
                generic: 1
            }
        );
        // Mode descriptions are effect-text only (post-separator)
        assert!(modal.mode_descriptions[0].contains("Create a 2/2"));
        assert!(modal.mode_descriptions[1].contains("Counter target spell"));
        // Two mode abilities parsed (not Unimplemented)
        assert_eq!(result.abilities.len(), 2);
        assert!(!matches!(
            result.abilities[0].effect,
            Effect::Unimplemented { .. }
        ));
    }

    #[test]
    fn spree_colored_mode_costs_parsed_correctly() {
        // Final Showdown has colored mode costs
        let text = "Spree (Choose one or more additional costs.)\n\
                     + {1} — All creatures lose all abilities until end of turn.\n\
                     + {1} — Choose a creature you control. It gains indestructible until end of turn.\n\
                     + {3}{W}{W} — Destroy all creatures.";
        let result = parse(text, "Final Showdown", &[Keyword::Spree], &["Instant"], &[]);
        let modal = result.modal.expect("should have modal");
        assert_eq!(modal.mode_count, 3);
        assert_eq!(modal.max_choices, 3);
        assert_eq!(modal.mode_costs.len(), 3);
        // Third mode: {3}{W}{W}
        if let ManaCost::Cost { shards, generic } = &modal.mode_costs[2] {
            assert_eq!(*generic, 3);
            assert_eq!(shards.len(), 2); // WW
        } else {
            panic!("Expected ManaCost::Cost for mode 2");
        }
    }

    #[test]
    fn collect_mode_asts_plus_prefix_extracts_cost_and_body() {
        let lines = vec![
            "Spree",
            "+ {2} — Draw a card.",
            "+ {R} — Deal 3 damage to target creature.",
        ];
        let modes = collect_mode_asts(&lines, 1);
        assert_eq!(modes.len(), 2);
        assert!(modes[0].mode_cost.is_some());
        assert_eq!(modes[0].body, "Draw a card.");
        assert!(modes[1].mode_cost.is_some());
    }

    #[test]
    fn collect_mode_asts_standard_bullet_has_no_mode_cost() {
        let lines = vec!["Choose one —", "• Draw a card.", "• Gain 3 life."];
        let modes = collect_mode_asts(&lines, 1);
        assert_eq!(modes.len(), 2);
        assert!(modes[0].mode_cost.is_none());
        assert!(modes[1].mode_cost.is_none());
    }

    #[test]
    fn standard_modal_spell_has_empty_mode_costs() {
        let text = "Choose one —\n• Draw a card.\n• Gain 3 life.";
        let result = parse(text, "Test Modal", &[], &["Instant"], &[]);
        let modal = result.modal.expect("should have modal");
        assert!(modal.mode_costs.is_empty());
    }

    #[test]
    fn collect_mode_asts_malformed_plus_line_stops_collection() {
        // A `+` line without valid mana cost should break mode collection
        let lines = vec![
            "Spree",
            "+ Draw a card.", // no mana cost after +
        ];
        let modes = collect_mode_asts(&lines, 1);
        assert!(modes.is_empty());
    }

    // --- Saga parser tests ---

    #[test]
    fn parse_roman_numeral_range() {
        assert_eq!(parse_roman_numeral("I"), Some(1));
        assert_eq!(parse_roman_numeral("ii"), Some(2));
        assert_eq!(parse_roman_numeral("III"), Some(3));
        assert_eq!(parse_roman_numeral("IV"), Some(4));
        assert_eq!(parse_roman_numeral("v"), Some(5));
        assert_eq!(parse_roman_numeral("VI"), Some(6));
        assert_eq!(parse_roman_numeral("VII"), Some(7));
        assert_eq!(parse_roman_numeral("VIII"), Some(8));
        assert_eq!(parse_roman_numeral("IX"), Some(9));
        assert_eq!(parse_roman_numeral("X"), Some(10));
        assert_eq!(parse_roman_numeral("XI"), None);
    }

    #[test]
    fn parse_chapter_line_single() {
        let (nums, effect) = parse_chapter_line("I — Draw a card.").unwrap();
        assert_eq!(nums, vec![1]);
        assert_eq!(effect, "Draw a card.");
    }

    #[test]
    fn parse_chapter_line_multi() {
        let (nums, effect) = parse_chapter_line("I, II — Target creature gets +2/+0.").unwrap();
        assert_eq!(nums, vec![1, 2]);
        assert_eq!(effect, "Target creature gets +2/+0.");
    }

    #[test]
    fn parse_chapter_line_hyphen_fallback() {
        let (nums, effect) = parse_chapter_line("III - Destroy target creature.").unwrap();
        assert_eq!(nums, vec![3]);
        assert_eq!(effect, "Destroy target creature.");
    }

    #[test]
    fn is_saga_chapter_extended() {
        assert!(is_saga_chapter("VI — Something"));
        assert!(is_saga_chapter("VII — Something"));
        assert!(is_saga_chapter("i — something"));
        assert!(!is_saga_chapter("Draw a card."));
    }

    #[test]
    fn parse_saga_the_eldest_reborn() {
        let oracle = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — Each opponent discards a card.\nII — Put target creature card from a graveyard onto the battlefield under your control.\nIII — Return target nonland permanent card from your graveyard to the battlefield under your control.";
        let result = parse_oracle_text(
            oracle,
            "The Eldest Reborn",
            &[],
            &["Enchantment".to_string()],
            &["Saga".to_string()],
        );

        // 3 chapter triggers
        assert_eq!(
            result.triggers.len(),
            3,
            "Expected 3 chapter triggers, got: {:?}",
            result.triggers.len()
        );
        for (i, trigger) in result.triggers.iter().enumerate() {
            assert_eq!(trigger.mode, TriggerMode::CounterAdded);
            let filter = trigger
                .counter_filter
                .as_ref()
                .expect("should have counter_filter");
            assert_eq!(
                filter.counter_type,
                crate::game::game_object::CounterType::Lore
            );
            assert_eq!(filter.threshold, Some((i + 1) as u32));
            assert_eq!(trigger.trigger_zones, vec![Zone::Battlefield]);
        }

        // 1 ETB replacement for lore counter
        assert!(
            !result.replacements.is_empty(),
            "Expected at least 1 replacement (ETB lore counter)"
        );
        let etb = &result.replacements[0];
        assert_eq!(etb.event, ReplacementEvent::Moved);
        assert_eq!(etb.valid_card, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn parse_saga_multi_chapter_line() {
        let oracle = "(Reminder text.)\nI, II — Draw a card.\nIII — Discard a card.";
        let result = parse_oracle_text(
            oracle,
            "Test Saga",
            &[],
            &["Enchantment".to_string()],
            &["Saga".to_string()],
        );

        // I and II share the same effect, III is separate = 3 triggers total
        assert_eq!(result.triggers.len(), 3);
        assert_eq!(
            result.triggers[0]
                .counter_filter
                .as_ref()
                .unwrap()
                .threshold,
            Some(1)
        );
        assert_eq!(
            result.triggers[1]
                .counter_filter
                .as_ref()
                .unwrap()
                .threshold,
            Some(2)
        );
        assert_eq!(
            result.triggers[2]
                .counter_filter
                .as_ref()
                .unwrap()
                .threshold,
            Some(3)
        );
    }

    #[test]
    fn parse_saga_subtypes_detection() {
        // Non-saga should NOT produce chapter triggers
        let oracle = "I — Draw a card.";
        let result =
            parse_oracle_text(oracle, "Not A Saga", &[], &["Enchantment".to_string()], &[]);
        assert!(
            result.triggers.is_empty(),
            "Non-saga subtypes should not produce chapter triggers"
        );
    }
}
