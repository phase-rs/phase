use serde::{Deserialize, Serialize};

use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, AdditionalCost,
    CastingRestriction, Comparator, DieResultBranch, Effect, ModalChoice, ReplacementDefinition,
    SolveCondition, SpellCastingOption, StaticDefinition, TriggerDefinition, TypedFilter,
};
use crate::types::keywords::Keyword;

use super::oracle_casting::{
    parse_additional_cost_line, parse_casting_restriction_line, parse_spell_casting_option_line,
};
use super::oracle_class::parse_class_oracle_text;
use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::parse_effect_chain;
pub use super::oracle_keyword::keyword_display_name;
use super::oracle_keyword::{extract_keyword_line, is_keyword_cost_line};
use super::oracle_level::parse_level_blocks;
use super::oracle_modal::{lower_oracle_block, parse_oracle_block, strip_ability_word};
use super::oracle_replacement::parse_replacement_line;
use super::oracle_saga::{is_saga_chapter, parse_saga_chapters};
use super::oracle_static::parse_static_line;
use super::oracle_trigger::parse_trigger_line;
use super::oracle_util::strip_reminder_text;

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

    // CR 710: Pre-parse leveler LEVEL blocks into counter-gated static abilities.
    let (level_statics, level_consumed) = parse_level_blocks(&lines);
    if !level_statics.is_empty() {
        result.statics.extend(level_statics);
    }

    let mut i = 0;

    while i < lines.len() {
        // CR 710: Skip lines already consumed by the leveler pre-parser.
        if level_consumed.contains(&i) {
            i += 1;
            continue;
        }

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

        // CR 702.6: Named equip variant — "<Flavor Name> — Equip {cost}"
        if let Some(idx) = lower
            .find(" \u{2014} equip")
            .or_else(|| lower.find(" - equip"))
        {
            let equip_part = line[idx..]
                .trim_start_matches(" \u{2014} ")
                .trim_start_matches(" - ");
            if let Some(ability) = try_parse_equip(equip_part) {
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
            i += 1;
            // CR 706: If the activated ability ends with "roll a dN", consume
            // subsequent d20 table lines and attach them as die result branches.
            if effect_text.to_lowercase().contains("roll a d") {
                i = attach_die_result_branches_to_chain(&mut def, &lines, i);
            }
            result.abilities.push(def);
            continue;
        }

        // Priority 5-6: Triggered abilities — starts with When/Whenever/At
        if lower.starts_with("when ") || lower.starts_with("whenever ") || lower.starts_with("at ")
        {
            let mut trigger = parse_trigger_line(&line, card_name);
            i += 1;
            // CR 706: If the trigger's effect ends with "roll a dN", consume
            // subsequent d20 table lines and attach them as die result branches.
            if lower.contains("roll a d") {
                if let Some(ref mut execute) = trigger.execute {
                    i = attach_die_result_branches_to_chain(execute, &lines, i);
                }
            }
            result.triggers.push(trigger);
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

        // CR 706: Die roll table — "Roll a dN" followed by "min—max | effect" lines.
        // Consumes the header + all table lines and produces a single RollDie ability.
        if let Some((def, next_i)) = try_parse_die_roll_table(
            &lines,
            i,
            &line,
            if is_spell {
                AbilityKind::Spell
            } else {
                AbilityKind::Activated
            },
        ) {
            result.abilities.push(def);
            i = next_i;
            continue;
        }

        // Priority 9: Imperative verb for instants/sorceries
        if is_spell {
            let mut def = parse_effect_chain(&line, AbilityKind::Spell);
            def.description = Some(line.to_string());
            i += 1;
            // CR 706: If the parsed chain ends with "roll a dN", consume
            // subsequent d20 table lines and attach them as die result branches.
            if lower.contains("roll a d") {
                i = attach_die_result_branches_to_chain(&mut def, &lines, i);
            }
            result.abilities.push(def);
            continue;
        }

        // Priority 12: Roman numeral chapters (saga) — skip
        if is_saga_chapter(&lower) {
            i += 1;
            continue;
        }

        // "The flashback cost is equal to its mana cost" → extract Flashback keyword
        if lower.contains("flashback cost")
            && lower.contains("equal to")
            && lower.contains("mana cost")
        {
            result.extracted_keywords.push(Keyword::Flashback(
                crate::types::mana::ManaCost::SelfManaCost,
            ));
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
                let mut trigger = parse_trigger_line(&effect_text, card_name);
                i += 1;
                // CR 706: Consume subsequent d20 table lines for triggered die rolls.
                if effect_lower.contains("roll a d") {
                    if let Some(ref mut execute) = trigger.execute {
                        i = attach_die_result_branches_to_chain(execute, &lines, i);
                    }
                }
                result.triggers.push(trigger);
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
pub(super) fn find_activated_colon(line: &str) -> Option<usize> {
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
/// Lines starting with "target" are spell effects, not statics — skip early.
pub(super) fn is_static_pattern(lower: &str) -> bool {
    // Spell effects targeting creatures/players are never static abilities.
    // They must reach the effect parser (Priority 9) for proper handling.
    if lower.starts_with("target") {
        return false;
    }

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
        || lower.starts_with("you control enchanted ")
        || lower.contains("play with the top card")
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
        // CR 509.1b: "must be blocked if able"
        || lower.contains("must be blocked")
        // CR 119.7: Lifegain prevention
        || lower.contains("can't gain life")
        // CR 702.8d: Flash-granting statics (exclude self-cast options like "you may cast this spell as though it had flash")
        || (lower.contains("as though it had flash") && !lower.starts_with("you may cast"))
        || lower.contains("as though they had flash")
        // Blocking rules
        || lower.contains("can block an additional")
        || lower.contains("can block any number")
        // Additional land drop
        || lower.contains("play an additional land")
        || lower.contains("play two additional lands")
        // CR 603.9: Trigger doubling — Panharmonicon-style statics
        || lower.contains("triggers an additional time")
}

pub(super) fn is_granted_static_line(lower: &str) -> bool {
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
pub(super) fn is_replacement_pattern(lower: &str) -> bool {
    lower.contains("would ")
        || lower.contains("prevent all")
        // "can't be prevented" is routed to effect parsing (Effect::AddRestriction),
        // not replacement parsing. It disables prevention rather than replacing events.
        || lower.contains("enters the battlefield tapped")
        || lower.contains("enters tapped")
        || lower.trim_end_matches('.').ends_with(" enter tapped")
        || (lower.contains("as ") && lower.contains("enters") && lower.contains("choose a"))
        || (lower.contains("enters") && lower.contains("counter"))
        // CR 707.9: "enter as a copy of" clone replacement effects
        || lower.contains("enter as a copy of")
}

/// Create an Unimplemented fallback ability.
pub(super) fn make_unimplemented(line: &str) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Unimplemented {
            name: "unknown".to_string(),
            description: Some(line.to_string()),
        },
    )
    .description(line.to_string())
}

/// Check if an AbilityDefinition (or its sub_ability chain) contains Unimplemented effects.
pub(super) fn has_unimplemented(def: &AbilityDefinition) -> bool {
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
pub(super) fn is_effect_sentence_candidate(lower: &str) -> bool {
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
pub(super) fn normalize_self_refs_for_static(text: &str, card_name: &str) -> String {
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

/// CR 706: Walk the sub_ability chain of a parsed trigger/ability to find the
/// terminal `RollDie { results: [] }` node and attach die result branches
/// from subsequent oracle text lines.
///
/// Returns the updated line index (past any consumed table lines).
fn attach_die_result_branches_to_chain(
    def: &mut AbilityDefinition,
    lines: &[&str],
    start_line: usize,
) -> usize {
    use super::oracle_effect::imperative::try_parse_die_result_line;

    // Walk to the end of the sub_ability chain to find the RollDie node.
    let roll_die = find_terminal_roll_die(def);
    let roll_die = match roll_die {
        Some(rd) => rd,
        None => return start_line,
    };

    // Consume subsequent d20 table lines
    let mut branches = Vec::new();
    let mut j = start_line;
    while j < lines.len() {
        let table_line = strip_reminder_text(lines[j].trim());
        if table_line.is_empty() {
            j += 1;
            continue;
        }
        if let Some((min, max, effect_text)) = try_parse_die_result_line(&table_line) {
            let effect_text = strip_die_table_flavor_label(effect_text);
            let branch_def = parse_effect_chain(effect_text, AbilityKind::Spell);
            branches.push(DieResultBranch {
                min,
                max,
                effect: Box::new(branch_def),
            });
            j += 1;
        } else {
            break;
        }
    }

    if !branches.is_empty() {
        if let Effect::RollDie {
            ref mut results, ..
        } = roll_die
        {
            *results = branches;
        }
    }

    j
}

/// Walk the sub_ability chain to find a terminal `RollDie { results: [] }` node.
fn find_terminal_roll_die(def: &mut AbilityDefinition) -> Option<&mut Effect> {
    // Check the current node first
    if matches!(&def.effect, Effect::RollDie { results, .. } if results.is_empty()) {
        return Some(&mut def.effect);
    }
    // Walk sub_ability chain
    if let Some(ref mut sub) = def.sub_ability {
        return find_terminal_roll_die(sub);
    }
    None
}

/// CR 706: Try to parse a die roll table starting at line `i`.
/// Detects "Roll a dN" followed by "min—max | effect" table lines.
/// Returns the consolidated `RollDie` ability definition and the next line index.
fn try_parse_die_roll_table(
    lines: &[&str],
    i: usize,
    line: &str,
    kind: AbilityKind,
) -> Option<(AbilityDefinition, usize)> {
    use super::oracle_effect::imperative::try_parse_die_result_line;

    let lower = line.to_lowercase();
    // Check for "roll a dN" pattern
    let sides = parse_roll_die_sides(&lower)?;

    // Look ahead for table lines
    let mut branches = Vec::new();
    let mut j = i + 1;
    while j < lines.len() {
        let table_line = strip_reminder_text(lines[j].trim());
        if table_line.is_empty() {
            j += 1;
            continue;
        }
        if let Some((min, max, effect_text)) = try_parse_die_result_line(&table_line) {
            // Strip optional flavor label like "Trapped! — "
            let effect_text = strip_die_table_flavor_label(effect_text);
            let branch_def = parse_effect_chain(effect_text, kind);
            branches.push(DieResultBranch {
                min,
                max,
                effect: Box::new(branch_def),
            });
            j += 1;
        } else {
            break;
        }
    }

    if branches.is_empty() {
        // No table lines follow — still a valid RollDie, just without branches
        let mut def = AbilityDefinition::new(
            kind,
            Effect::RollDie {
                sides,
                results: vec![],
            },
        );
        def.description = Some(line.to_string());
        return Some((def, i + 1));
    }

    let mut def = AbilityDefinition::new(
        kind,
        Effect::RollDie {
            sides,
            results: branches,
        },
    );
    def.description = Some(line.to_string());
    Some((def, j))
}

/// CR 706: Parse die side count from "roll a dN" patterns in lowercased text.
fn parse_roll_die_sides(lower: &str) -> Option<u8> {
    let rest = lower
        .strip_prefix("roll a d")
        .or_else(|| lower.strip_prefix("rolls a d"))?;
    let rest = rest.trim_end_matches('.');
    if let Ok(sides) = rest.parse::<u8>() {
        return Some(sides);
    }
    // Word-form: "roll a dfour-sided die", etc. — not a real pattern.
    // The "d" prefix doesn't precede word forms; handle separately if needed.
    None
}

/// Strip optional flavor labels from d20 table effect text.
/// E.g., "Trapped! — You lose 3 life" → "You lose 3 life"
fn strip_die_table_flavor_label(text: &str) -> &str {
    // Look for " — " (em dash U+2014) pattern at the start
    if let Some(idx) = text.find(" \u{2014} ") {
        let before = &text[..idx];
        // Flavor labels are short (1-4 words) and often end with "!"
        if before.split_whitespace().count() <= 4 {
            return &text[idx + " \u{2014} ".len()..];
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ModalSelectionConstraint, QuantityExpr, TargetFilter};
    use crate::types::mana::ManaCost;
    use crate::types::replacements::ReplacementEvent;
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
    fn end_to_end_toxic_keyword_no_unimplemented() {
        // End-to-end: "Toxic 2" with MTGJSON keyword name "toxic" should be
        // fully handled — no Unimplemented effects in output
        let r = parse_with_keyword_names(
            "Toxic 2",
            "Glistener Elf",
            &["toxic"],
            &["Creature"],
            &["Phyrexian", "Elf", "Warrior"],
        );
        let has_unimplemented = r.abilities.iter().any(|a| {
            matches!(
                a.effect,
                crate::types::ability::Effect::Unimplemented { .. }
            )
        });
        assert!(
            !has_unimplemented,
            "Toxic keyword line should not produce Unimplemented effects"
        );
    }

    #[test]
    fn end_to_end_typecycling_no_unimplemented() {
        // "Plainscycling {2}" with MTGJSON keyword name should not produce Unimplemented
        let r = parse_with_keyword_names(
            "Plainscycling {2}",
            "Twisted Abomination",
            &["plainscycling"],
            &["Creature"],
            &["Zombie", "Mutant"],
        );
        let has_unimplemented = r.abilities.iter().any(|a| {
            matches!(
                a.effect,
                crate::types::ability::Effect::Unimplemented { .. }
            )
        });
        assert!(
            !has_unimplemented,
            "Typecycling keyword line should not produce Unimplemented effects"
        );
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

    // ── Feature #1: Reflexive triggers ("when you do") ──────────────

    #[test]
    fn reflexive_trigger_when_you_do_sentence_split() {
        // "you may pay {1}. When you do, draw a card" — sentence-split produces
        // a chunk starting with "When you do, ..." that strip_if_you_do_conditional handles.
        let r = parse(
            "Whenever ~ attacks, you may pay {1}. When you do, draw a card.",
            "Test Card",
            &[],
            &["Creature"],
            &[],
        );
        assert!(!r.triggers.is_empty(), "should parse the trigger");
        let abilities = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        // First ability is PayCost (optional), second is Draw with IfYouDo condition
        assert!(
            matches!(abilities.effect, Effect::PayCost { .. }),
            "first effect should be PayCost, got {:?}",
            abilities.effect,
        );
        let sub = abilities
            .sub_ability
            .as_ref()
            .expect("should have sub_ability");
        assert_eq!(
            sub.condition,
            Some(crate::types::ability::AbilityCondition::IfYouDo),
            "sub-ability should have IfYouDo condition"
        );
        assert!(
            matches!(sub.effect, Effect::Draw { .. }),
            "sub effect should be Draw, got {:?}",
            sub.effect,
        );
    }

    #[test]
    fn reflexive_trigger_when_you_do_comma_split() {
        // "when you do, attach ~ to it" — comma-separated, starts_prefix_clause
        // must prevent splitting at the comma boundary.
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain(
            "When you do, attach Ancestral Katana to it",
            crate::types::ability::AbilityKind::Spell,
        );
        assert_eq!(
            def.condition,
            Some(crate::types::ability::AbilityCondition::IfYouDo),
            "should detect IfYouDo condition"
        );
        assert!(
            matches!(def.effect, Effect::Attach { .. }),
            "effect should be Attach, got {:?}",
            def.effect,
        );
    }

    // ── Feature #2: "Cast without paying" effects ───────────────────

    #[test]
    fn cast_without_paying_mana_cost() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("cast it without paying its mana cost");
        assert!(
            matches!(
                effect,
                Effect::CastFromZone {
                    target: TargetFilter::ParentTarget,
                    without_paying_mana_cost: true,
                    ..
                }
            ),
            "expected CastFromZone with ParentTarget + without_paying, got {:?}",
            effect,
        );
    }

    #[test]
    fn cast_that_card() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("cast that card");
        assert!(
            matches!(
                effect,
                Effect::CastFromZone {
                    target: TargetFilter::ParentTarget,
                    without_paying_mana_cost: false,
                    ..
                }
            ),
            "expected CastFromZone with ParentTarget + paying, got {:?}",
            effect,
        );
    }

    #[test]
    fn cast_clause_splits_correctly() {
        // "exile the top card of your library, then cast it without paying its mana cost"
        // "cast it..." should be a separate clause, not merged with "exile..."
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain(
            "exile the top card of your library, then cast it without paying its mana cost",
            crate::types::ability::AbilityKind::Spell,
        );
        // First effect is ChangeZone (exile), sub is CastFromZone
        assert!(
            matches!(def.effect, Effect::ChangeZone { .. }),
            "first effect should be ChangeZone, got {:?}",
            def.effect,
        );
        let sub = def
            .sub_ability
            .as_ref()
            .expect("should have sub_ability for cast");
        assert!(
            matches!(
                sub.effect,
                Effect::CastFromZone {
                    without_paying_mana_cost: true,
                    ..
                }
            ),
            "sub effect should be CastFromZone with without_paying, got {:?}",
            sub.effect,
        );
    }

    // ── Feature #3: "For each" iteration ────────────────────────────

    #[test]
    fn for_each_prefix_creates_token() {
        // "for each opponent, create a 2/2 black Zombie creature token"
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain(
            "for each opponent, create a 2/2 black Zombie creature token",
            crate::types::ability::AbilityKind::Spell,
        );
        assert!(
            def.repeat_for.is_some(),
            "repeat_for should be set for 'for each opponent'"
        );
        assert!(
            matches!(def.effect, Effect::Token { .. }),
            "inner effect should be Token, got {:?}",
            def.effect,
        );
    }

    #[test]
    fn for_each_prefix_exiles() {
        // "for each opponent, exile up to one target nonland permanent"
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain(
            "for each opponent, exile up to one target nonland permanent",
            crate::types::ability::AbilityKind::Spell,
        );
        assert!(def.repeat_for.is_some(), "repeat_for should be set");
        assert!(
            matches!(def.effect, Effect::ChangeZone { .. }),
            "inner effect should be ChangeZone (exile), got {:?}",
            def.effect,
        );
    }

    #[test]
    fn for_each_trailing_still_works() {
        // Existing "for each" trailing pattern should still work
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("draw a card for each creature you control");
        assert!(
            matches!(
                effect,
                Effect::Draw {
                    count: QuantityExpr::Ref { .. }
                }
            ),
            "trailing 'for each' should produce dynamic Draw, got {:?}",
            effect,
        );
    }

    // ── Coverage batch: keyword granting ──────────────────────────────

    #[test]
    fn gain_haste_keyword_granting() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("gain haste");
        assert!(
            matches!(effect, Effect::GenericEffect { .. }),
            "expected GenericEffect for 'gain haste', got {:?}",
            effect,
        );
    }

    #[test]
    fn gain_flying_until_end_of_turn() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("gain flying until end of turn");
        assert!(
            matches!(effect, Effect::GenericEffect { .. }),
            "expected GenericEffect for 'gain flying until end of turn', got {:?}",
            effect,
        );
    }

    #[test]
    fn gain_trample_and_haste() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("gain trample and haste");
        assert!(
            matches!(effect, Effect::GenericEffect { .. }),
            "expected GenericEffect for 'gain trample and haste', got {:?}",
            effect,
        );
    }

    // ── Coverage batch: investigate ───────────────────────────────────

    #[test]
    fn investigate_parses() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("investigate");
        assert!(
            matches!(effect, Effect::Investigate),
            "expected Investigate, got {:?}",
            effect,
        );
    }

    #[test]
    fn investigate_twice_chains() {
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("investigate twice", AbilityKind::Spell);
        assert!(
            matches!(def.effect, Effect::Investigate),
            "first effect should be Investigate, got {:?}",
            def.effect,
        );
        let sub = def
            .sub_ability
            .as_ref()
            .expect("investigate twice should chain via sub_ability");
        assert!(
            matches!(sub.effect, Effect::Investigate),
            "sub effect should be Investigate, got {:?}",
            sub.effect,
        );
    }

    #[test]
    fn proliferate_twice_chains() {
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("proliferate twice", AbilityKind::Spell);
        assert!(
            matches!(def.effect, Effect::Proliferate),
            "first effect should be Proliferate, got {:?}",
            def.effect,
        );
        assert!(
            def.sub_ability.is_some(),
            "proliferate twice should chain via sub_ability"
        );
    }

    #[test]
    fn investigate_three_times_chains() {
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("investigate three times", AbilityKind::Spell);
        assert!(matches!(def.effect, Effect::Investigate));
        let sub1 = def.sub_ability.as_ref().expect("should have first sub");
        assert!(matches!(sub1.effect, Effect::Investigate));
        let sub2 = sub1.sub_ability.as_ref().expect("should have second sub");
        assert!(matches!(sub2.effect, Effect::Investigate));
    }

    // ── Coverage batch: gold tokens ──────────────────────────────────

    #[test]
    fn create_gold_token() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("create a Gold token");
        assert!(
            matches!(effect, Effect::Token { ref name, .. } if name == "Gold"),
            "expected Gold Token, got {:?}",
            effect,
        );
    }

    // ── Coverage batch: become the monarch ────────────────────────────

    #[test]
    fn become_the_monarch_imperative() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("become the monarch");
        assert!(
            matches!(effect, Effect::BecomeMonarch),
            "expected BecomeMonarch, got {:?}",
            effect,
        );
    }

    #[test]
    fn you_become_the_monarch_subject() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("you become the monarch");
        assert!(
            matches!(effect, Effect::BecomeMonarch),
            "expected BecomeMonarch, got {:?}",
            effect,
        );
    }

    // ── Coverage batch: prevent damage ────────────────────────────────

    #[test]
    fn prevent_next_3_damage() {
        use crate::parser::oracle_effect::parse_effect;
        use crate::types::ability::PreventionAmount;
        let effect =
            parse_effect("prevent the next 3 damage that would be dealt to any target this turn");
        match effect {
            Effect::PreventDamage {
                amount: PreventionAmount::Next(3),
                ..
            } => {}
            _ => panic!("expected PreventDamage with Next(3), got {:?}", effect),
        }
    }

    #[test]
    fn prevent_all_combat_damage() {
        use crate::parser::oracle_effect::parse_effect;
        use crate::types::ability::{PreventionAmount, PreventionScope};
        let effect = parse_effect("prevent all combat damage that would be dealt this turn");
        match effect {
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                scope: PreventionScope::CombatDamage,
                ..
            } => {}
            _ => panic!(
                "expected PreventDamage All + CombatDamage, got {:?}",
                effect
            ),
        }
    }

    // ── Coverage batch: play from exile ────────────────────────────────

    #[test]
    fn play_that_card() {
        use crate::parser::oracle_effect::parse_effect;
        use crate::types::ability::CardPlayMode;
        let effect = parse_effect("play that card");
        match effect {
            Effect::CastFromZone {
                mode: CardPlayMode::Play,
                target: TargetFilter::ParentTarget,
                ..
            } => {}
            _ => panic!("expected CastFromZone with Play mode, got {:?}", effect),
        }
    }

    #[test]
    fn cast_uses_cast_mode() {
        use crate::parser::oracle_effect::parse_effect;
        use crate::types::ability::CardPlayMode;
        let effect = parse_effect("cast that card");
        match effect {
            Effect::CastFromZone {
                mode: CardPlayMode::Cast,
                ..
            } => {}
            _ => panic!("expected CastFromZone with Cast mode, got {:?}", effect),
        }
    }

    // ── Coverage batch: shuffle and put on top ─────────────────────────

    #[test]
    fn shuffle_and_put_on_top() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("shuffle your library and put that card on top");
        assert!(
            matches!(effect, Effect::Shuffle { .. }),
            "expected Shuffle for 'shuffle and put on top', got {:?}",
            effect,
        );
    }

    #[test]
    fn emergent_growth_routes_to_spell_not_static() {
        // Emergent Growth: compound pump + must-be-blocked should route to spell
        // effect parsing, not static parsing.
        let parsed = parse(
            "Target creature gets +5/+5 until end of turn and must be blocked this turn if able.",
            "Emergent Growth",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(
            !parsed.abilities.is_empty(),
            "Emergent Growth should produce a spell ability, got abilities={:?}, statics={:?}",
            parsed.abilities,
            parsed.statics,
        );
        assert!(
            parsed.statics.is_empty(),
            "Emergent Growth should NOT produce static abilities, got {:?}",
            parsed.statics,
        );
    }
}
