use serde::{Deserialize, Serialize};

use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, AdditionalCost,
    CastingRestriction, Comparator, DieResultBranch, Effect, ModalChoice, ReplacementDefinition,
    SolveCondition, SpellCastingOption, StaticDefinition, TargetFilter, TriggerDefinition,
    TypedFilter,
};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::oracle_casting::{
    parse_additional_cost_line, parse_casting_restriction_line, parse_spell_casting_option_line,
};
use super::oracle_class::parse_class_oracle_text;
use super::oracle_condition::parse_restriction_condition;
use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::{parse_effect_chain, parse_effect_chain_with_context, ParseContext};
pub use super::oracle_keyword::keyword_display_name;
use super::oracle_keyword::{
    extract_keyword_line, is_keyword_cost_line, parse_keyword_from_oracle,
};
use super::oracle_level::parse_level_blocks;
use super::oracle_modal::{
    lower_oracle_block, parse_oracle_block, strip_ability_word, strip_ability_word_with_name,
};
use super::oracle_nom::error::parse_or_unimplemented;
use super::oracle_replacement::parse_replacement_line;
use super::oracle_saga::{is_saga_chapter, parse_saga_chapters};
use super::oracle_static::{parse_static_line, parse_static_line_multi};
use super::oracle_trigger::parse_trigger_line;
use super::oracle_util::{
    normalize_card_name_refs, parse_mana_symbols, parse_subtype, strip_reminder_text, TextPair,
};

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
    /// CR 207.2c + CR 601.2f: Strive per-target surcharge cost.
    /// "This spell costs {X} more to cast for each target beyond the first."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strive_cost: Option<ManaCost>,
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
/// Map a known ability word name to a typed `StaticCondition`.
/// Returns `None` for unrecognized ability words (Landfall, Constellation, etc.
/// don't have implicit conditions — their trigger text encodes the condition).
///
/// Covers:
/// - Threshold: 7+ cards in graveyard
/// - Metalcraft: 3+ artifacts you control
/// - Delirium: 4+ card types in graveyard
/// - Spell mastery: 2+ instant/sorcery in graveyard
/// - Revolt: a permanent you controlled left the battlefield this turn
fn ability_word_to_condition(word: &str) -> Option<crate::types::ability::StaticCondition> {
    use crate::types::ability::{
        ControllerRef, CountScope, QuantityExpr, QuantityRef, StaticCondition, TargetFilter,
        TypeFilter, TypedFilter, ZoneRef,
    };

    match word {
        "threshold" => Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::GraveyardSize,
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 7 },
        }),
        "metalcraft" => Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
                    ),
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 3 },
        }),
        "delirium" => Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::CardTypesInGraveyards {
                    scope: CountScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 4 },
        }),
        "spell mastery" => Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Graveyard,
                    card_types: vec![TypeFilter::Instant, TypeFilter::Sorcery],
                    scope: CountScope::Controller,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 2 },
        }),
        "revolt" => {
            // Revolt: "a permanent you controlled left the battlefield this turn"
            // Uses the per-turn zone-change tracking on GameState.
            // Mapped to a QuantityComparison checking permanents_left_battlefield > 0.
            // The tracking field already exists as part of the general zone-change tracking.
            Some(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::PermanentsLeftBattlefieldThisTurn,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        }
        "max speed" => Some(StaticCondition::HasMaxSpeed),
        _ => None,
    }
}

/// Convert an ability-word `StaticCondition` to an `AbilityCondition` for spell effects.
fn ability_word_to_ability_condition(
    cond: &Option<crate::types::ability::StaticCondition>,
) -> Option<crate::types::ability::AbilityCondition> {
    use crate::types::ability::{AbilityCondition, StaticCondition};
    match cond.as_ref()? {
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => Some(AbilityCondition::QuantityCheck {
            lhs: lhs.clone(),
            comparator: *comparator,
            rhs: rhs.clone(),
        }),
        StaticCondition::HasMaxSpeed => Some(AbilityCondition::HasMaxSpeed),
        _ => None,
    }
}

/// Convert an ability-word condition to a `TriggerCondition`.
/// All known ability words use `StaticCondition::QuantityComparison`, which maps
/// directly to `TriggerCondition::QuantityComparison`.
fn ability_word_to_trigger_condition(
    word: &str,
) -> Option<crate::types::ability::TriggerCondition> {
    use crate::types::ability::{StaticCondition, TriggerCondition};
    match ability_word_to_condition(word)? {
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => Some(TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        }),
        StaticCondition::HasMaxSpeed => Some(TriggerCondition::HasMaxSpeed),
        _ => None,
    }
}

/// triggered, static, replacement, spell effect, modal, loyalty, etc.).
///
/// `mtgjson_keyword_names` are the raw lowercased keyword names from MTGJSON
/// (e.g. `["flying", "protection"]`). Used to identify keyword-only lines
/// and to avoid re-extracting keywords MTGJSON already provides.
#[tracing::instrument(
    level = "info",
    skip(oracle_text, mtgjson_keyword_names, types, subtypes)
)]
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
        strive_cost: None,
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

    // CR 207.2c + CR 601.2f: Pre-parse Strive ability word cost before main loop.
    // Strive lines have the form: "Strive — This spell costs {X} more to cast for each
    // target beyond the first." — extract the per-target surcharge cost.
    for raw in &lines {
        let stripped = strip_reminder_text(raw.trim());
        if let Some(effect_text) = strip_ability_word(&stripped) {
            let effect_lower = effect_text.to_lowercase();
            if let Some(rest_lower) = effect_lower.strip_prefix("this spell costs ") {
                // Use original-case text for mana symbol parsing ({U} not {u}).
                let rest_original = &effect_text[effect_text.len() - rest_lower.len()..];
                if let Some((mana_part, _)) =
                    rest_original.split_once(" more to cast for each target beyond the first")
                {
                    if let Some((cost, _)) = parse_mana_symbols(mana_part) {
                        result.strive_cost = Some(cost);
                        break;
                    }
                }
            }
        }
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

        // Priority 0: Semicolon-separated keyword lines (e.g., "Defender; reach").
        // Oracle text uses semicolons exclusively to separate keywords on a single line.
        // The colon guard prevents splitting activated ability lines like "{T}: Draw a card".
        if line.contains(';') && !line.contains(':') {
            let parts: Vec<&str> = line
                .split(';')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if parts.len() > 1 {
                let all_keywords = parts
                    .iter()
                    .all(|part| extract_keyword_line(part, mtgjson_keyword_names).is_some());
                if all_keywords {
                    for part in &parts {
                        if let Some(extracted) = extract_keyword_line(part, mtgjson_keyword_names) {
                            result.extracted_keywords.extend(extracted);
                        }
                    }
                    i += 1;
                    continue;
                }
            }
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
        // Guard: "{Keyword} abilities you activate cost {N} less" is a static ability,
        // not a keyword line. Don't let keyword extraction consume it.
        let lower_guard = line.to_lowercase();
        let is_ability_cost_static =
            lower_guard.contains("abilities you activate cost") && lower_guard.contains("less");
        if !is_ability_cost_static {
            if let Some(extracted) = extract_keyword_line(&line, mtgjson_keyword_names) {
                result.extracted_keywords.extend(extracted);
                i += 1;
                continue;
            }
        }

        let lower = line.to_lowercase();

        // Normalize card self-references for static parsing (replace card name with ~)
        let static_line = normalize_self_refs_for_static(&line, card_name);

        if lower == "start your engines!" || lower == "start your engines" {
            result.extracted_keywords.push(Keyword::StartYourEngines);
            i += 1;
            continue;
        }

        if lower == "your speed can increase beyond 4."
            || lower == "your speed can increase beyond 4"
        {
            if let Some(static_def) = parse_static_line(&static_line) {
                result.statics.push(static_def);
                i += 1;
                continue;
            }
        }

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
        let tp = TextPair::new(&line, &lower);
        if let Some(idx) = tp.find(" \u{2014} equip").or_else(|| tp.find(" - equip")) {
            let equip_part = tp
                .split_at(idx)
                .1
                .original
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
            // B20: Handle compound "can't win/lose" lines by splitting
            if lower.contains("can't win the game") && lower.contains("can't lose the game") {
                for clause in static_line.split(" and ") {
                    let trimmed = clause.trim().trim_end_matches('.');
                    if !trimmed.is_empty() {
                        let clause_dot = format!("{trimmed}.");
                        if let Some(sd) = parse_static_line(&clause_dot) {
                            result.statics.push(sd);
                        }
                    }
                }
                i += 1;
                continue;
            }
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

        // Priority 3c: Channel — "Channel — {cost}, Discard this card: {effect}" (CR 207.2c + CR 602.1)
        if let Some(rest) = lower
            .strip_prefix("channel — ")
            .or_else(|| lower.strip_prefix("channel -- "))
        {
            if let Some(colon_pos) = find_activated_colon(rest) {
                let prefix_len = line.len() - rest.len();
                let cost_text = line[prefix_len..prefix_len + colon_pos].trim();
                let effect_text = line[prefix_len + colon_pos + 1..].trim();
                let (effect_text, constraints) = strip_activated_constraints(effect_text);
                let cost = parse_oracle_cost(cost_text);
                let mut def = parse_effect_chain(&effect_text, AbilityKind::Activated);
                def.cost = Some(cost);
                // CR 207.2c: Channel is an ability word; the underlying ability activates from hand.
                def.activation_zone = Some(Zone::Hand);
                def.description = Some(line.to_string());
                if constraints.sorcery_speed() {
                    def.sorcery_speed = true;
                }
                if !constraints.restrictions.is_empty() {
                    def.activation_restrictions = constraints.restrictions;
                }
                // CR 601.2f: Extract self-referential cost reduction from the terminal
                // sub_ability in the chain (it may be several levels deep).
                extract_cost_reduction_from_chain(&mut def);
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
            // Normalize card name in cost text (e.g., "Exile Wilson from your graveyard" → "Exile ~ from your graveyard")
            let normalized_cost_text = normalize_self_refs_for_static(cost_text, card_name);
            let cost = parse_oracle_cost(&normalized_cost_text);

            // Try parsing without normalization first; if unimplemented, retry
            // with self-reference normalization (e.g., "Marwyn's power" → "~'s power")
            let mut def = parse_effect_chain(&effect_text, AbilityKind::Activated);
            if has_unimplemented(&def) {
                let normalized_effect = normalize_self_refs_for_static(&effect_text, card_name);
                if normalized_effect != effect_text {
                    let alt = parse_effect_chain(&normalized_effect, AbilityKind::Activated);
                    if !has_unimplemented(&alt) {
                        def = alt;
                    }
                }
            }
            def.cost = Some(cost);
            def.description = Some(line.to_string());
            if constraints.sorcery_speed() {
                def.sorcery_speed = true;
            }
            if !constraints.restrictions.is_empty() {
                def.activation_restrictions = constraints.restrictions;
            }
            // CR 601.2f: Extract self-referential cost reduction from the terminal
            // sub_ability in the chain (may be several levels deep).
            extract_cost_reduction_from_chain(&mut def);
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

        // Priority 6b: Ability-word-prefixed triggers (e.g., "Heroic — Whenever ...",
        // "Constellation — Whenever ..."). Must intercept BEFORE is_static_pattern and
        // is_replacement_pattern checks, which would otherwise match on keywords like
        // "prevent" in the effect text and misroute the line.
        if let Some((aw_name, effect_text)) = strip_ability_word_with_name(&line) {
            let effect_lower = effect_text.to_lowercase();
            if effect_lower.starts_with("when ")
                || effect_lower.starts_with("whenever ")
                || effect_lower.starts_with("at ")
            {
                let mut trigger = parse_trigger_line(&effect_text, card_name);
                // B7: Attach ability-word condition as fallback when extract_if_condition
                // doesn't recognize the intervening-if pattern.
                if trigger.condition.is_none() {
                    trigger.condition = ability_word_to_trigger_condition(&aw_name);
                }
                i += 1;
                if effect_lower.contains("roll a d") {
                    if let Some(ref mut execute) = trigger.execute {
                        i = attach_die_result_branches_to_chain(execute, &lines, i);
                    }
                }
                result.triggers.push(trigger);
                continue;
            }
        }

        // Priority 7: Static/continuous patterns
        // CR 611.2a + CR 611.3a: On permanents, "creatures you control get +1/+1"
        // is a static ability (CR 611.3a). On instants/sorceries, lines with an
        // explicit duration ("until end of turn", "this turn") are one-shot
        // continuous effects from spell resolution (CR 611.2a) and must reach the
        // effect parser at Priority 9. Damage-verb lines are also deferred because
        // parse_effect_chain handles embedded statics via split_clause_sequence.
        if is_static_pattern(&lower) {
            // Guard: ability-word-prefixed trigger lines (e.g., "Flurry — Whenever...")
            // handled above at Priority 6b. The check below is kept as a defensive
            // guard for any edge cases that reach Priority 7.
            let is_ability_word_trigger = strip_ability_word(&line).is_some_and(|stripped| {
                let sl = stripped.to_lowercase();
                sl.starts_with("when ") || sl.starts_with("whenever ") || sl.starts_with("at ")
            });
            let defer_to_effect_parser = is_ability_word_trigger
                || (is_spell
                    && (((lower.contains(" deals ") || lower.contains(" deal "))
                        && lower.contains(" damage"))
                        || lower.contains("until end of turn")
                        || lower.contains("until your next turn")
                        || lower.contains("this turn")));
            if !defer_to_effect_parser {
                // B7: Ability-word-prefixed static lines — strip prefix and attach condition.
                // Must happen here (Priority 7) because Priority 9 (spell catch-all) would
                // otherwise consume the line before Priority 14 for instants/sorceries.
                if let Some((aw_name, effect_text)) = strip_ability_word_with_name(&line) {
                    let effect_static = normalize_self_refs_for_static(&effect_text, card_name);
                    if let Some(mut static_def) = parse_static_line(&effect_static) {
                        if static_def.condition.is_none() {
                            if let Some(cond) = ability_word_to_condition(&aw_name) {
                                static_def.condition = Some(cond);
                            }
                        }
                        static_def.description = Some(line.to_string());
                        result.statics.push(static_def);
                        i += 1;
                        continue;
                    }
                }
                // B20: Handle compound "can't win/lose" lines by splitting
                // at " and " so both CantWinTheGame and CantLoseTheGame emit.
                // CR 104.3a / CR 104.3b: Both restrictions must be independent statics.
                if lower.contains("can't win the game") && lower.contains("can't lose the game") {
                    for clause in static_line.split(" and ") {
                        let trimmed = clause.trim().trim_end_matches('.');
                        if !trimmed.is_empty() {
                            let clause_dot = format!("{trimmed}.");
                            if let Some(sd) = parse_static_line(&clause_dot) {
                                result.statics.push(sd);
                            }
                        }
                    }
                    i += 1;
                    continue;
                }
                // Compound clause: casting time restriction + per-turn limit joined by " and "
                // E.g., Fires of Invention: "You can cast spells only during your turn and
                // you can cast no more than two spells each turn."
                // CR 117.1a + CR 604.1: Both restrictions are independent statics.
                if lower.contains("only during your turn")
                    && lower.contains(" and ")
                    && lower.contains("each turn")
                {
                    for clause in static_line.split(" and ") {
                        let trimmed = clause.trim().trim_end_matches('.');
                        if !trimmed.is_empty() {
                            let clause_dot = format!("{trimmed}.");
                            if let Some(sd) = parse_static_line(&clause_dot) {
                                result.statics.push(sd);
                            }
                        }
                    }
                    i += 1;
                    continue;
                }
                // Use parse_static_line_multi to capture compound statics
                // (e.g., "attacks or blocks each combat if able" → MustAttack + MustBlock).
                let multi = parse_static_line_multi(&static_line);
                if !multi.is_empty() {
                    result.statics.extend(multi);
                    i += 1;
                    continue;
                }
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
                        enter_transformed: false,
                        under_your_control: false,
                        enter_tapped: false,
                        enters_attacking: false,
                    },
                )
                .description(line.to_string()),
            );
            i += 1;
            continue;
        }

        // Priority 8b-defiler: "As an additional cost to cast [color] permanent spells,
        // you may pay N life." + next line "Those spells cost {C} less to cast."
        // This is a static ability on the permanent, not a self-cost for this spell.
        if lower.starts_with("as an additional cost to cast ")
            && !lower.contains("this spell")
            && lower.contains("you may pay")
            && lower.contains(" life")
        {
            if let Some(static_def) =
                parse_defiler_cost_reduction(&lower, i + 1 < lines.len(), || {
                    lines.get(i + 1).map(|l| l.to_lowercase())
                })
            {
                result.statics.push(static_def);
                // Consume both lines (cost line + reduction line)
                i += 2;
                continue;
            }
        }

        // Priority 8b: "As an additional cost to cast this spell"
        if lower.starts_with("as an additional cost") {
            result.additional_cost = parse_additional_cost_line(&lower, &line);
            i += 1;
            continue;
        }

        // Priority 8c-strive: Skip strive lines (cost already extracted in pre-parse above).
        // Must run before Priority 9 (spell imperative catch-all) which would otherwise
        // consume the entire "Strive — This spell costs..." line as an unimplemented ability.
        if result.strive_cost.is_some() {
            if let Some(effect_text) = strip_ability_word(&line) {
                let effect_lower = effect_text.to_lowercase();
                if effect_lower.starts_with("this spell costs ")
                    && effect_lower.contains("more to cast for each target beyond the first")
                {
                    i += 1;
                    continue;
                }
            }
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

        // Harmonize {cost} — parse mana cost from Oracle text.
        // Must run before the spell imperative catch-all (priority 9) so the line
        // is intercepted as a keyword, not parsed as an effect.
        // MTGJSON keywords array only says "Harmonize" (no cost), so we extract cost here.
        // Format: "Harmonize {cost} (reminder text)" — space-separated.
        // Note: When MTGJSON provides "Harmonize" in keywords, extract_keyword_line at
        // priority 1b already handles this. This is a fallback for test/edge cases.
        if lower.starts_with("harmonize ") {
            if let Some(harmonize_kw) = parse_harmonize_keyword(&line) {
                result.extracted_keywords.push(harmonize_kw);
                i += 1;
                continue;
            }
        }

        // Priority 9: Imperative verb for instants/sorceries
        if is_spell {
            // B7: Strip ability-word prefix and attach condition for spell effects.
            let (aw_condition, effect_line) =
                if let Some((aw_name, effect_text)) = strip_ability_word_with_name(&line) {
                    (ability_word_to_condition(&aw_name), effect_text)
                } else {
                    (None, line.clone())
                };
            let mut def = parse_effect_chain_with_context(
                &effect_line,
                AbilityKind::Spell,
                &ParseContext {
                    subject: None,
                    card_name: Some(card_name.to_string()),
                },
            );
            def.description = Some(line.to_string());
            if def.condition.is_none() {
                def.condition = ability_word_to_ability_condition(&aw_condition);
            }
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

        // CR 702.49d: Commander ninjutsu is not in MTGJSON keywords — extract explicitly.
        if lower.starts_with("commander ninjutsu ") {
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // CR 702.138: Escape — parse cost and exile count from Oracle text.
        // Must run before is_keyword_cost_line so the em-dash format is intercepted.
        if lower.starts_with("escape") && line.contains('\u{2014}') {
            if let Some(escape_kw) = parse_escape_keyword(&line) {
                result.extracted_keywords.push(escape_kw);
                i += 1;
                continue;
            }
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

        // Priority 13c: Vehicle tier lines "N+ | keyword(s)" — skip (conditional stat grant)
        if is_vehicle_tier_line(&lower) {
            i += 1;
            continue;
        }

        // Priority 13d: "Activate only..." constraint — skip
        if lower.starts_with("activate ") || lower.starts_with("activate only") {
            i += 1;
            continue;
        }

        // Priority 14: Ability word — strip prefix and re-classify effect.
        // B7: Known ability words (Threshold, Metalcraft, Delirium, Spell mastery, Revolt)
        // are mapped to typed conditions and attached to the resulting definition.
        if let Some((aw_name, effect_text)) = strip_ability_word_with_name(&line) {
            let aw_condition = ability_word_to_condition(&aw_name);
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
                if let Some(mut static_def) = parse_static_line(&effect_static) {
                    // B7: Attach ability word condition to static definition
                    if static_def.condition.is_none() {
                        if let Some(cond) = aw_condition.clone() {
                            static_def.condition = Some(cond);
                        }
                    }
                    result.statics.push(static_def);
                    i += 1;
                    continue;
                }
            }
            // Try as effect
            let def = parse_effect_chain_with_context(
                &effect_text,
                AbilityKind::Spell,
                &ParseContext {
                    subject: None,
                    card_name: Some(card_name.to_string()),
                },
            );
            if !has_unimplemented(&def) {
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 14a: "damage can't be prevented" → AddRestriction effect
        if lower.contains("damage") && lower.contains("can't be prevented") {
            let def = parse_effect_chain_with_context(
                &line,
                AbilityKind::Spell,
                &ParseContext {
                    subject: None,
                    card_name: Some(card_name.to_string()),
                },
            );
            if !has_unimplemented(&def) {
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 14b: Try parsing as effect even for non-spells
        if is_effect_sentence_candidate(&lower) {
            let def = parse_effect_chain_with_context(
                &line,
                AbilityKind::Spell,
                &ParseContext {
                    subject: None,
                    card_name: Some(card_name.to_string()),
                },
            );
            if !has_unimplemented(&def) {
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 15: Fallback — attempt all major branch parsers via nom alt()
        // to collect error trace (D-12) before falling back to Unimplemented.
        let fallback_effect = dispatch_line_nom(&line, &static_line, card_name);
        result
            .abilities
            .push(make_unimplemented_with_effect(&line, fallback_effect));
        i += 1;
    }

    result
}

/// Try to parse "Equip {cost}" or "Equip — {cost}" lines.
/// Caller must verify the line starts with "equip" (case-insensitive) before calling.
fn try_parse_equip(line: &str) -> Option<AbilityDefinition> {
    // Caller already verified lower.starts_with("equip") — strip 5-char prefix.
    // "equip" is always ASCII so byte length == char length.
    let rest = line.get("equip".len()..)?.trim();
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
    if let Some(after_open) = trimmed.strip_prefix('[') {
        if let Some((inner, rest)) = after_open.split_once(']') {
            if let Some(effect_text) = rest.trim().strip_prefix(':') {
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
    if let Some((prefix, effect_text)) = trimmed.split_once(':') {
        if let Some(amount) = parse_loyalty_number(prefix) {
            // Verify it looks like a loyalty prefix (starts with +, −, –, -, or is "0")
            let first_char = prefix.trim().chars().next()?;
            if first_char == '+'
                || first_char == '−'
                || first_char == '–'
                || first_char == '-'
                || prefix.trim() == "0"
            {
                let effect_text = effect_text.trim();
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

/// CR 601.2f: Walk the sub_ability chain to find a terminal `Unimplemented` that is
/// a cost reduction pattern. If found, remove it from the chain and return the parsed
/// `CostReduction`. The cost reduction may be several levels deep (e.g., Boseiju has
/// SearchLibrary → ChangeZone → ChangeZone → Unimplemented(cost reduction)).
fn extract_cost_reduction_from_chain(def: &mut AbilityDefinition) {
    if let Some(reduction) = strip_cost_reduction_node(&mut def.sub_ability) {
        def.cost_reduction = Some(reduction);
    }
}

/// Recursively walk the sub_ability chain. If a node is an `Unimplemented` cost
/// reduction, remove it and return the parsed `CostReduction`.
fn strip_cost_reduction_node(
    slot: &mut Option<Box<AbilityDefinition>>,
) -> Option<crate::types::ability::CostReduction> {
    let sub = slot.as_mut()?;
    if let Effect::Unimplemented {
        description: Some(ref desc),
        ..
    } = *sub.effect
    {
        if let Some(reduction) = super::oracle_cost::try_parse_cost_reduction(&desc.to_lowercase())
        {
            // Remove this node, promote its child (usually None).
            *slot = sub.sub_ability.take();
            return Some(reduction);
        }
    }
    // Recurse into the chain.
    strip_cost_reduction_node(&mut sub.sub_ability)
}

/// Find the position of ":" that indicates an activated ability cost/effect split.
/// The left side must look like a cost (contains "{", or starts with cost-like words,
/// or is a loyalty marker).
pub(super) fn find_activated_colon(line: &str) -> Option<usize> {
    let colon_pos = find_top_level_colon(line)?;
    let prefix = &line[..colon_pos];

    // Contains mana symbols
    if prefix.contains('{') {
        return Some(colon_pos);
    }

    // Starts with cost-like words (all ASCII — case-insensitive prefix check)
    let trimmed = prefix.trim();
    let cost_starters = [
        "sacrifice",
        "discard",
        "pay",
        "remove",
        "exile",
        "tap",
        "untap",
        "put",
    ];
    // Only lowercase when needed (skipped entirely if '{' was found above)
    let lower_prefix = trimmed.to_lowercase();
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
        let tp = TextPair::new(&remaining, &lower);

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

        if let Some(idx) = tp.rfind("activate only if ") {
            if idx == 0 {
                let mut condition_text = remaining["activate only if ".len()..].trim().to_string();
                strip_once_per_turn_suffix(&mut condition_text, &mut constraints.restrictions);
                remaining.clear();
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        condition: parse_restriction_condition(&condition_text),
                    });
                break;
            }
            if lower[..idx].ends_with(". ") {
                let mut condition_text = remaining[idx + "activate only if ".len()..]
                    .trim()
                    .to_string();
                strip_once_per_turn_suffix(&mut condition_text, &mut constraints.restrictions);
                remaining = remaining[..idx]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        condition: parse_restriction_condition(&condition_text),
                    });
                continue;
            }
        }

        if let Some(idx) = tp.rfind("activate only from ") {
            if idx == 0 || lower[..idx].ends_with(". ") {
                let restriction_text = remaining[idx + "activate only from ".len()..]
                    .trim()
                    .to_string();
                remaining = remaining[..idx]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                let full_text = format!("from {restriction_text}");
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        condition: parse_restriction_condition(&full_text),
                    });
                continue;
            }
        }

        if let Some(idx) = tp.rfind("activate only ") {
            if idx == 0 || lower[..idx].ends_with(". ") {
                let restriction_text = remaining[idx + "activate only ".len()..].trim().to_string();
                remaining = remaining[..idx]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        condition: parse_restriction_condition(&restriction_text),
                    });
                continue;
            }
        }

        if let Some(idx) = tp.rfind("activate no more than ") {
            if idx == 0 || lower[..idx].ends_with(". ") {
                let restriction_text = remaining[idx + "activate no more than ".len()..]
                    .trim()
                    .to_string();
                remaining = remaining[..idx]
                    .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                    .to_string();
                let full_text = format!("no more than {restriction_text}");
                constraints
                    .restrictions
                    .push(ActivationRestriction::RequiresCondition {
                        condition: parse_restriction_condition(&full_text),
                    });
                continue;
            }
        }

        break;
    }

    (remaining, constraints)
}

/// Strip "and only once each turn" / "and only once" compound suffixes from a condition_text
/// extracted from "activate only if [condition_text]", pushing the corresponding
/// `OnlyOnceEachTurn`/`OnlyOnce` restriction.
///
/// Uses the `text.len() - suffix.len()` offset idiom (CR 602.5b): all suffixes are ASCII,
/// so byte-length slicing is safe.
fn strip_once_per_turn_suffix(
    condition_text: &mut String,
    restrictions: &mut Vec<ActivationRestriction>,
) {
    let lower = condition_text.to_lowercase();
    if lower.ends_with(" and only once each turn") {
        let stripped_len = condition_text.len() - " and only once each turn".len();
        *condition_text = condition_text[..stripped_len]
            .trim_end_matches(|c: char| c == ',' || c.is_whitespace())
            .to_string();
        restrictions.push(ActivationRestriction::OnlyOnceEachTurn);
    } else if lower.ends_with(" and only once") {
        let stripped_len = condition_text.len() - " and only once".len();
        *condition_text = condition_text[..stripped_len]
            .trim_end_matches(|c: char| c == ',' || c.is_whitespace())
            .to_string();
        restrictions.push(ActivationRestriction::OnlyOnce);
    }
}

/// Check if a line looks like a static/continuous ability.
/// Lines starting with "target" are spell effects, not statics — skip early.
/// Substring patterns that indicate a static ability line.
/// Used by `is_static_pattern` for table-driven classification.
const STATIC_CONTAINS_PATTERNS: &[&str] = &[
    "gets +",
    "gets -",
    "get +",
    "get -",
    "have ",
    "has ",
    "can't be blocked",
    "can't attack",
    "can't block",
    "can't be countered",
    "can't be the target",
    "can't be sacrificed",
    "doesn't untap",
    "don't untap",
    "attacks each combat if able",
    "can block only creatures with flying",
    "no maximum hand size",
    "may choose not to untap",
    "play with the top card",
    "cost {",
    "costs {",
    "cost less",
    "cost more",
    "costs less",
    "costs more",
    "is the chosen type",
    "lose all abilities",
    "power is equal to",
    "power and toughness are each equal to",
    // CR 509.1b: "must be blocked if able"
    "must be blocked",
    // CR 119.7: Lifegain prevention
    "can't gain life",
    // CR 104.3a/b: Win/lose the game restrictions
    "can't win the game",
    "can't lose the game",
    // Blocking rules
    "can block an additional",
    "can block any number",
    // Additional land drop
    "play an additional land",
    "play two additional lands",
    // CR 603.9: Trigger doubling
    "triggers an additional time",
    // CR 604.3: Zone-based restrictions
    "can't enter the battlefield",
    "can't cast spells from",
    // CR 101.2: Turn/phase-scoped casting prohibitions
    "can't cast spells during",
    // CR 117.1a + CR 604.1: Casting only during turn
    "can cast spells only during",
    // CR 702.127a: Skip draw step
    "skip your draw step",
    // CR 102.4: Maximum hand size
    "maximum hand size",
    // CR 613: Life total can't change
    "life total can't change",
    // CR 613: Damage modification statics
    "assigns combat damage equal to its toughness",
    "as though it weren't blocked",
    // CR 702.4b: Vigilance-style statics
    "attacking doesn't cause",
    // CR 702.8d: Flash-granting (filtered by prefix guard below)
    "as though they had flash",
];

/// Prefix patterns that indicate a static ability line.
const STATIC_PREFIX_PATTERNS: &[&str] = &[
    "as long as ",
    "enchanted ",
    "equipped ",
    "you control enchanted ",
    "all creatures ",
    "all permanents ",
    "other ",
    "each creature ",
    "cards in ",
    "creatures you control ",
    "each player ",
    "spells you cast ",
    "spells your opponents cast ",
    "you may look at the top card of your library",
    "once during each of your turns, you may cast",
    "a deck can have",
    "nonland ",
    "noncreature ",
    // CR 305.7: Land type-changing statics
    "nonbasic lands are ",
    "each land is a ",
    "all lands are ",
    "lands you control are ",
];

pub(super) fn is_static_pattern(lower: &str) -> bool {
    // Spell effects targeting creatures/players are never static abilities.
    // They must reach the effect parser (Priority 9) for proper handling.
    if lower.strip_prefix("target").is_some() {
        return false;
    }

    // Table-driven substring matching
    if STATIC_CONTAINS_PATTERNS
        .iter()
        .any(|pat| lower.contains(pat))
    {
        return true;
    }

    // Table-driven prefix matching
    if STATIC_PREFIX_PATTERNS
        .iter()
        .any(|pat| lower.strip_prefix(pat).is_some())
    {
        return true;
    }

    // Compound patterns requiring multiple conditions
    // CR 702.8d: Flash-granting statics (exclude self-cast options)
    if lower.contains("as though it had flash") && lower.strip_prefix("you may cast").is_none() {
        return true;
    }
    // "enters with" but not counter-related (which is replacement)
    if lower.contains("enters with ") && !lower.contains("counter") {
        return true;
    }
    // "creatures your opponents control" but not "enter tapped" (which is replacement)
    if lower
        .strip_prefix("creatures your opponents control ")
        .is_some()
        && !lower.trim_end_matches('.').ends_with("enter tapped")
    {
        return true;
    }
    // CR 604.2 + CR 601.2a: Graveyard cast/play permission
    if lower.strip_prefix("you may play").is_some() && lower.contains("from your graveyard") {
        return true;
    }
    if lower.strip_prefix("you may cast").is_some() && lower.contains("from your graveyard") {
        return true;
    }
    // CR 601.3c: Casting restrictions by name/property
    if lower.contains("can't cast") && lower.contains("spells") {
        return true;
    }
    // CR 101.2 + CR 604.1: Per-turn casting limits
    if lower.contains("no more than") && lower.contains("spells") && lower.contains("each turn") {
        return true;
    }

    false
}

/// Prefix patterns for granted static lines (e.g., 'Enchanted creature has "..."').
const GRANTED_STATIC_PREFIXES: &[&str] = &[
    "enchanted ",
    "equipped ",
    "all ",
    "creatures ",
    "lands ",
    "other ",
    "you ",
    "players ",
    "each player ",
];

/// Substring patterns for granted ability text.
const GRANTED_STATIC_VERBS: &[&str] = &[" has \"", " have \"", " gains \"", " gain \""];

pub(super) fn is_granted_static_line(lower: &str) -> bool {
    GRANTED_STATIC_PREFIXES
        .iter()
        .any(|p| lower.strip_prefix(p).is_some())
        && GRANTED_STATIC_VERBS.iter().any(|v| lower.contains(v))
}

/// Check if a line looks like a replacement effect.
/// Detect vehicle tier lines like "7+ | Flying" or "12+ | {3}{W}".
/// These are conditional stat/ability grants based on total power of crewing creatures.
fn is_vehicle_tier_line(lower: &str) -> bool {
    if let Some(pipe_pos) = lower.find(" | ") {
        let prefix = lower[..pipe_pos].trim();
        // Must be "N+" pattern: one or more digits followed by '+'
        if let Some(num_part) = prefix.strip_suffix('+') {
            return !num_part.is_empty() && num_part.chars().all(|c| c.is_ascii_digit());
        }
    }
    false
}

/// Substring patterns that indicate a replacement effect line.
const REPLACEMENT_CONTAINS_PATTERNS: &[&str] = &[
    "would ",
    "prevent all",
    // "can't be prevented" is routed to effect parsing (Effect::AddRestriction),
    // not replacement parsing. It disables prevention rather than replacing events.
    "enters the battlefield tapped",
    "enters tapped",
    // CR 707.9: "enter as a copy of" clone replacement effects
    "enter as a copy of",
];

pub(super) fn is_replacement_pattern(lower: &str) -> bool {
    // Table-driven substring matching
    if REPLACEMENT_CONTAINS_PATTERNS
        .iter()
        .any(|pat| lower.contains(pat))
    {
        return true;
    }

    // Suffix pattern: "... enter tapped."
    if lower.trim_end_matches('.').ends_with(" enter tapped") {
        return true;
    }
    // "as ... enters ... choose a" pattern
    if lower.contains("as ") && lower.contains("enters") && lower.contains("choose a") {
        return true;
    }
    // "enters ... counter" pattern
    if lower.contains("enters") && lower.contains("counter") {
        return true;
    }
    // CR 614.1a: Mana production replacement
    if lower.contains("tapped for mana") && lower.contains("instead") {
        return true;
    }

    false
}

/// Primary nom-based dispatcher for Oracle text lines.
///
/// Classifies lines by structural pattern using nom `alt()` and routes to
/// the appropriate sub-parser. All unmatched lines produce `Effect::Unimplemented`
/// with `VerboseError` diagnostic traces (D-12/D-13) showing which parser
/// categories were attempted and why none matched.
///
/// This is the primary dispatch path — called from the main loop for lines
/// that don't match multi-line constructs (modal blocks, die tables, etc.).
fn dispatch_line_nom(line: &str, static_line: &str, card_name: &str) -> Effect {
    use nom::branch::alt;
    use nom::bytes::complete::tag_no_case;
    use nom::combinator::fail;
    use nom::Parser;

    use super::oracle_nom::error::OracleResult;

    // Structural recognizers: each attempts to match the line and classify
    // it into a parser category. Builds the nom VerboseError trace so
    // unmatched lines have diagnostic information about which branches
    // were attempted.
    fn try_trigger_prefix(input: &str) -> OracleResult<'_, Effect> {
        alt((
            tag_no_case("when "),
            tag_no_case("whenever "),
            tag_no_case("at "),
        ))
        .map(|_: &str| Effect::Unimplemented {
            name: "trigger_structure".into(),
            description: Some(format!(
                "Trigger prefix matched but line failed trigger parser: {input}"
            )),
        })
        .parse(input)
    }

    fn try_static_prefix(input: &str) -> OracleResult<'_, Effect> {
        let lower = input.to_lowercase();
        if is_static_pattern(&lower) {
            Ok((
                "",
                Effect::Unimplemented {
                    name: "static_structure".into(),
                    description: Some(format!(
                        "Static pattern matched but line failed static parser: {input}"
                    )),
                },
            ))
        } else {
            fail::<_, Effect, _>().parse(input)
        }
    }

    fn try_replacement_prefix(input: &str) -> OracleResult<'_, Effect> {
        let lower = input.to_lowercase();
        if is_replacement_pattern(&lower) {
            Ok((
                "",
                Effect::Unimplemented {
                    name: "replacement_structure".into(),
                    description: Some(format!(
                        "Replacement pattern matched but line failed replacement parser: {input}"
                    )),
                },
            ))
        } else {
            fail::<_, Effect, _>().parse(input)
        }
    }

    fn try_effect_prefix(input: &str) -> OracleResult<'_, Effect> {
        let lower = input.to_lowercase();
        if is_effect_sentence_candidate(&lower) {
            Ok((
                "",
                Effect::Unimplemented {
                    name: "effect_structure".into(),
                    description: Some(format!(
                        "Effect sentence candidate but line failed effect parser: {input}"
                    )),
                },
            ))
        } else {
            fail::<_, Effect, _>().parse(input)
        }
    }

    // First, try the actual sub-parsers for lines that match structural patterns.
    // This promotes nom dispatch from fallback to primary: if a sub-parser succeeds,
    // we use its result directly rather than requiring the priority chain to catch it.
    let lower = line.to_lowercase();

    // Try trigger parser for trigger-prefixed lines
    if lower.strip_prefix("when ").is_some()
        || lower.strip_prefix("whenever ").is_some()
        || lower.strip_prefix("at ").is_some()
    {
        let trigger = parse_trigger_line(line, card_name);
        if let Some(ref execute) = trigger.execute {
            if !has_unimplemented(execute) {
                return *execute.effect.clone();
            }
        }
    }

    // Try static parser for static-pattern lines
    if is_static_pattern(&lower) {
        if let Some(_sd) = parse_static_line(static_line) {
            // Static was parsed successfully by the sub-parser — return a marker
            // so the caller knows dispatch_line_nom handled it (the actual StaticDefinition
            // is collected by the priority chain's static handling code)
        }
    }

    // Try effect parser for imperative sentence lines
    if is_effect_sentence_candidate(&lower) {
        let def = parse_effect_chain_with_context(
            line,
            AbilityKind::Spell,
            &ParseContext {
                subject: None,
                card_name: Some(card_name.to_string()),
            },
        );
        if !has_unimplemented(&def) {
            return *def.effect;
        }
    }

    // All sub-parsers failed — produce VerboseError diagnostic trace
    parse_or_unimplemented(line, |input| {
        alt((
            try_trigger_prefix,
            try_static_prefix,
            try_replacement_prefix,
            try_effect_prefix,
        ))
        .parse(input)
    })
}

/// Create an Unimplemented fallback ability with a nom-provided diagnostic effect.
///
/// If `dispatch_line_nom` succeeded in parsing the line (found a match in one of
/// the nom branches), this wraps the effect in an AbilityDefinition. Otherwise
/// it creates an Unimplemented with the error trace from the nom dispatch.
fn make_unimplemented_with_effect(line: &str, effect: Effect) -> AbilityDefinition {
    if !matches!(effect, Effect::Unimplemented { .. }) {
        // Nom dispatch found a match — use the effect directly.
        // This shouldn't normally happen (priority checks above should have caught it),
        // but handles edge cases where the nom fallback finds something.
        return AbilityDefinition::new(AbilityKind::Spell, effect).description(line.to_string());
    }
    tracing::warn!(oracle_text = line, "unimplemented ability line");
    AbilityDefinition::new(AbilityKind::Spell, effect).description(line.to_string())
}

/// Create an Unimplemented fallback ability.
pub(super) fn make_unimplemented(line: &str) -> AbilityDefinition {
    tracing::warn!(oracle_text = line, "unimplemented ability line");
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
    if matches!(*def.effect, Effect::Unimplemented { .. }) {
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
/// Prefix patterns for imperative verb effect sentences.
const EFFECT_IMPERATIVE_PREFIXES: &[&str] = &[
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

/// Prefix patterns for subject-led effect sentences.
const EFFECT_SUBJECT_PREFIXES: &[&str] = &[
    "all ", "if ", "it ", "target ", "that ", "they ", "this ", "those ", "you ",
];

pub(super) fn is_effect_sentence_candidate(lower: &str) -> bool {
    EFFECT_IMPERATIVE_PREFIXES
        .iter()
        .chain(EFFECT_SUBJECT_PREFIXES.iter())
        .any(|prefix| lower.strip_prefix(prefix).is_some())
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

        // CR 205.3m: Normalize plural subtypes to canonical singular form
        let rest_trimmed = rest.trim();
        let subtype = parse_subtype(rest_trimmed)
            .map(|(canonical, _)| canonical)
            .unwrap_or_else(|| {
                super::oracle_effect::capitalize(rest_trimmed.trim_end_matches('s'))
            });

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

/// Parse the Defiler cycle two-line pattern into a DefilerCostReduction static.
///
/// Line 1: "as an additional cost to cast [color] permanent spells, you may pay [N] life."
/// Line 2: "those spells cost {C} less to cast."
///
/// Returns a StaticDefinition with DefilerCostReduction mode, or None if the pattern
/// doesn't match.
fn parse_defiler_cost_reduction(
    lower: &str,
    has_next_line: bool,
    next_line_lower: impl FnOnce() -> Option<String>,
) -> Option<StaticDefinition> {
    // Extract color from "to cast [color] permanent spells"
    let after_cast = lower.strip_prefix("as an additional cost to cast ")?;
    let perm_pos = after_cast.find(" permanent spell")?;
    let color_word = after_cast[..perm_pos].trim();
    let color = match color_word {
        "white" => ManaColor::White,
        "blue" => ManaColor::Blue,
        "black" => ManaColor::Black,
        "red" => ManaColor::Red,
        "green" => ManaColor::Green,
        _ => return None,
    };

    // Extract life cost from "you may pay [N] life"
    let pay_pos = lower.find("you may pay ")?;
    let after_pay = &lower[pay_pos + "you may pay ".len()..];
    let (life_cost, _) = super::oracle_util::parse_number(after_pay)?;

    // Parse the second line for mana reduction
    if !has_next_line {
        return None;
    }
    let next_lower = next_line_lower()?;
    let next_trimmed = next_lower.trim().trim_end_matches('.');

    // "those spells cost {X} less to cast"
    let cost_rest = next_trimmed.strip_prefix("those spells cost ")?;
    let less_pos = cost_rest.find(" less to cast")?;
    let mana_text = cost_rest[..less_pos].trim();

    // Sanity check: the mana text should be a single color symbol like "{g}"
    if !mana_text.starts_with('{') {
        return None;
    }

    // Map color to corresponding ManaCostShard for the reduction
    let shard = match color {
        ManaColor::White => ManaCostShard::White,
        ManaColor::Blue => ManaCostShard::Blue,
        ManaColor::Black => ManaCostShard::Black,
        ManaColor::Red => ManaCostShard::Red,
        ManaColor::Green => ManaCostShard::Green,
    };
    let mana_reduction = ManaCost::Cost {
        shards: vec![shard],
        generic: 0,
    };

    Some(
        StaticDefinition::new(StaticMode::DefilerCostReduction {
            color,
            life_cost,
            mana_reduction,
        })
        .affected(TargetFilter::SelfRef)
        .description(format!(
            "As an additional cost to cast {} permanent spells, you may pay {} life. Those spells cost less to cast.",
            color_word, life_cost
        )),
    )
}

/// Normalize self-references in a line for static ability parsing.
///
/// Replaces the card name (and legendary short name) with `~`
/// so that `parse_static_line` can match patterns like "~ has".
pub(super) fn normalize_self_refs_for_static(text: &str, card_name: &str) -> String {
    normalize_card_name_refs(text, card_name)
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
    if matches!(&*def.effect, Effect::RollDie { results, .. } if results.is_empty()) {
        return Some(&mut *def.effect);
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

/// CR 702.138: Parse "Escape—{cost}, Exile N other cards from your graveyard."
/// Returns `Keyword::Escape { cost, exile_count }` or None if the line doesn't match.
fn parse_escape_keyword(line: &str) -> Option<Keyword> {
    let (_, after_dash) = line.split_once('\u{2014}')?;
    let after_dash = after_dash.trim();

    // Extract mana cost from the start (e.g. "{W}" or "{3}{B}{B}")
    let (cost, rest) = super::oracle_util::parse_mana_symbols(after_dash)?;

    // After the cost, expect ", Exile N other cards from your graveyard"
    let rest = rest.trim_start_matches(',').trim();
    let rest_lower = rest.to_lowercase();
    let exile_part = rest_lower.strip_prefix("exile ")?;

    // Parse the number ("two", "five", "eight", etc.)
    let (exile_count, _) = super::oracle_util::parse_number(exile_part)?;

    Some(Keyword::Escape { cost, exile_count })
}

/// Parse "Harmonize {cost} (reminder text)" from Oracle text.
/// Format: "Harmonize {5}{R}{R} (You may cast this card from ...)"
/// The cost is space-separated from "Harmonize", with optional reminder text in parens.
/// Returns `Keyword::Harmonize(cost)` or None if the line doesn't match.
fn parse_harmonize_keyword(line: &str) -> Option<Keyword> {
    let rest = line
        .strip_prefix("Harmonize ")
        .or_else(|| line.strip_prefix("harmonize "))?;

    // Strip reminder text in parentheses if present
    let cost_str = if let Some(paren_start) = rest.find('(') {
        rest[..paren_start].trim()
    } else {
        rest.trim()
    };

    if cost_str.is_empty() {
        return None;
    }

    let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str);
    Some(Keyword::Harmonize(cost))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        ModalSelectionConstraint, QuantityExpr, QuantityRef, TargetFilter, TypeFilter, TypedFilter,
    };
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
            *r.abilities[0].effect,
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
            *r.abilities[0].effect,
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
        assert_eq!(r.statics[0].mode, crate::types::statics::StaticMode::Shroud);
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
            crate::types::statics::StaticMode::MayLookAtTopOfLibrary
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
                value: Keyword::Flying,
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
            crate::types::statics::StaticMode::NoMaximumHandSize
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
            crate::types::statics::StaticMode::BlockRestriction
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
            *draw.effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
        let no_activate_tail = draw
            .sub_ability
            .as_ref()
            .is_none_or(|tail| !matches!(*tail.effect, Effect::Unimplemented { ref name, .. } if name == "activate"));
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
            *r.abilities[0].effect,
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
            .condition(crate::types::ability::ParsedCondition::OpponentSearchedLibraryThisTurn)
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(!matches!(
            *r.abilities[0].effect,
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
                .condition(crate::types::ability::ParsedCondition::FirstSpellThisGame)]
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
            *r.abilities[0].effect,
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
            [ActivationRestriction::RequiresCondition {
                condition: Some(
                    crate::types::ability::ParsedCondition::YouControlLandSubtypeAny { .. }
                )
            }]
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
    fn parses_activate_only_if_condition_and_only_once_each_turn() {
        // CR 602.5b: "Activate only if [condition] and only once each turn" must produce
        // both a RequiresCondition restriction (with the condition) and OnlyOnceEachTurn.
        // Tests the general pattern, not a single card.
        use crate::types::ability::{ParsedCondition, PlayerFilter};
        let r = parse(
            "{1}{R}: Put a +1/+1 counter on this creature. Activate only if an opponent lost life this turn and only once each turn.",
            "Test Card",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let restrictions = &r.abilities[0].activation_restrictions;
        assert!(
            restrictions.contains(&ActivationRestriction::OnlyOnceEachTurn),
            "expected OnlyOnceEachTurn restriction"
        );
        assert!(
            restrictions.iter().any(|r| matches!(
                r,
                ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::PlayerCountAtLeast {
                        filter: PlayerFilter::OpponentLostLife,
                        minimum: 1,
                    })
                }
            )),
            "expected RequiresCondition with OpponentLostLife"
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
                *a.effect,
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
                *a.effect,
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
            *r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
        assert!(matches!(
            *r.abilities[1].effect,
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
            *r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
        assert!(matches!(
            *r.abilities[1].effect,
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
            *r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
        assert!(matches!(
            *r.abilities[1].effect,
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
            *execute.effect,
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
            *execute.mode_abilities[0].effect,
            Effect::SearchLibrary { .. }
        ));
        let search_sub = execute.mode_abilities[0]
            .sub_ability
            .as_ref()
            .expect("search mode should have change-zone followup");
        assert!(matches!(
            *search_sub.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                ..
            }
        ));

        assert!(matches!(
            *execute.mode_abilities[1].effect,
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
            *execute.mode_abilities[2].effect,
            Effect::SearchLibrary { .. }
        ));
        let search_sub = execute.mode_abilities[2]
            .sub_ability
            .as_ref()
            .expect("play games mode should have change-zone followup");
        assert!(matches!(
            *search_sub.effect,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                ..
            }
        ));

        assert!(matches!(
            *execute.mode_abilities[3].effect,
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
            *modal_def.mode_abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
        assert!(matches!(
            *modal_def.mode_abilities[1].effect,
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
            *result.abilities[0].effect,
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
    fn ghirapur_grand_prix_put_counter_uses_speed_quantity() {
        let oracle = "When you planeswalk here, all players start their engines! (If you have no speed, it starts at 1. It increases once on each of your turns when an opponent loses life. Max speed is 4.)\nAt the beginning of your end step, put X +1/+1 counters on target creature you control, where X is your speed.\nWhen you planeswalk away from Ghirapur Grand Prix, each player with the highest speed among players creates three Treasure tokens.";
        let result = parse_oracle_text(
            oracle,
            "Ghirapur Grand Prix",
            &[],
            &[],
            &["Avishkar".to_string()],
        );

        let end_step_trigger = result
            .triggers
            .iter()
            .find(|trigger| {
                trigger
                    .description
                    .as_deref()
                    .is_some_and(|d| d.contains("put X +1/+1 counters"))
            })
            .expect("expected end-step trigger");
        let execute = end_step_trigger.execute.as_ref().expect("expected execute");
        assert!(matches!(
            *execute.effect,
            Effect::PutCounter {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Speed,
                },
                ..
            }
        ));
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
        // First ability is PayCost (optional), second is Draw with WhenYouDo condition.
        // CR 603.12: "when you do" is a reflexive trigger, distinct from "if you do".
        assert!(
            matches!(*abilities.effect, Effect::PayCost { .. }),
            "first effect should be PayCost, got {:?}",
            abilities.effect,
        );
        let sub = abilities
            .sub_ability
            .as_ref()
            .expect("should have sub_ability");
        assert_eq!(
            sub.condition,
            Some(crate::types::ability::AbilityCondition::WhenYouDo),
            "sub-ability should have WhenYouDo condition"
        );
        assert!(
            matches!(*sub.effect, Effect::Draw { .. }),
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
            Some(crate::types::ability::AbilityCondition::WhenYouDo),
            "should detect WhenYouDo condition"
        );
        assert!(
            matches!(*def.effect, Effect::Attach { .. }),
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
        // First effect is ExileTop (dedicated top-of-library exile), sub is CastFromZone
        assert!(
            matches!(*def.effect, Effect::ExileTop { .. }),
            "first effect should be ExileTop, got {:?}",
            def.effect,
        );
        let sub = def
            .sub_ability
            .as_ref()
            .expect("should have sub_ability for cast");
        assert!(
            matches!(
                *sub.effect,
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
            matches!(*def.effect, Effect::Token { .. }),
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
            matches!(*def.effect, Effect::ChangeZone { .. }),
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
    fn investigate_twice_uses_repeat_for() {
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("investigate twice", AbilityKind::Spell);
        assert!(
            matches!(*def.effect, Effect::Investigate),
            "first effect should be Investigate, got {:?}",
            def.effect,
        );
        // CR 609.3: "twice" → repeat_for = Fixed(2), resolver handles repetition.
        assert_eq!(def.repeat_for, Some(QuantityExpr::Fixed { value: 2 }));
        assert!(def.sub_ability.is_none());
    }

    #[test]
    fn proliferate_twice_uses_repeat_for() {
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("proliferate twice", AbilityKind::Spell);
        assert!(
            matches!(*def.effect, Effect::Proliferate),
            "first effect should be Proliferate, got {:?}",
            def.effect,
        );
        assert_eq!(def.repeat_for, Some(QuantityExpr::Fixed { value: 2 }));
        assert!(def.sub_ability.is_none());
    }

    #[test]
    fn investigate_three_times_uses_repeat_for() {
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("investigate three times", AbilityKind::Spell);
        assert!(matches!(*def.effect, Effect::Investigate));
        // CR 609.3: "three times" → repeat_for = Fixed(3), not cloned sub_ability chain.
        assert_eq!(
            def.repeat_for,
            Some(QuantityExpr::Fixed { value: 3 }),
            "expected repeat_for=Fixed(3), got {:?}",
            def.repeat_for
        );
        assert!(
            def.sub_ability.is_none(),
            "should not clone sub_abilities — resolver handles repetition"
        );
    }

    #[test]
    fn repeat_suffix_preserves_sub_ability_chain() {
        // Verifies that "twice" suffix doesn't drop sub_abilities from compound effects.
        // "scry 2 twice" → Scry with repeat_for=Fixed(2), no cloned chain.
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("scry 2 twice", AbilityKind::Spell);
        assert!(
            matches!(*def.effect, Effect::Scry { .. }),
            "expected Scry, got {:?}",
            def.effect,
        );
        assert_eq!(def.repeat_for, Some(QuantityExpr::Fixed { value: 2 }));
    }

    #[test]
    fn repeat_suffix_on_draw_card() {
        use crate::parser::oracle_effect::parse_effect_chain;
        let def = parse_effect_chain("draw a card twice", AbilityKind::Spell);
        // "draw a card" should parse as Draw, with repeat_for = 2
        assert!(matches!(
            &*def.effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
        assert_eq!(def.repeat_for, Some(QuantityExpr::Fixed { value: 2 }));
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
    fn put_that_card_on_top_abbreviated() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("put that card on top");
        assert!(
            matches!(effect, Effect::PutAtLibraryPosition { .. }),
            "expected PutAtLibraryPosition for abbreviated form, got {:?}",
            effect,
        );
    }

    #[test]
    fn put_them_on_top_abbreviated() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("put them on top");
        assert!(
            matches!(effect, Effect::PutAtLibraryPosition { .. }),
            "expected PutAtLibraryPosition for 'put them on top', got {:?}",
            effect,
        );
    }

    #[test]
    fn put_on_top_of_library_long_form() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("put it on top of your library");
        assert!(
            matches!(effect, Effect::PutAtLibraryPosition { .. }),
            "expected PutAtLibraryPosition for long form, got {:?}",
            effect,
        );
    }

    #[test]
    fn enlightened_tutor_chain() {
        // CR 701.24b: "search, reveal, then shuffle and put that card on top"
        // Should produce: SearchLibrary → Shuffle → PutAtLibraryPosition (no ChangeZone→Hand)
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;
        let chain = parse_effect_chain(
            "Search your library for an artifact or enchantment card, reveal it, then shuffle and put that card on top",
            AbilityKind::Spell,
        );
        // First effect: SearchLibrary with reveal
        assert!(
            matches!(*chain.effect, Effect::SearchLibrary { reveal: true, .. }),
            "expected SearchLibrary with reveal, got {:?}",
            chain.effect,
        );
        // Sub_ability: Shuffle
        let sub1 = chain
            .sub_ability
            .as_ref()
            .expect("should have sub_ability (Shuffle)");
        assert!(
            matches!(*sub1.effect, Effect::Shuffle { .. }),
            "expected Shuffle as second effect, got {:?}",
            sub1.effect,
        );
        // Sub_ability of Shuffle: PutOnTop
        let sub2 = sub1
            .sub_ability
            .as_ref()
            .expect("should have sub_ability (PutAtLibraryPosition)");
        assert!(
            matches!(*sub2.effect, Effect::PutAtLibraryPosition { .. }),
            "expected PutAtLibraryPosition as third effect, got {:?}",
            sub2.effect,
        );
        // No further sub_abilities
        assert!(
            sub2.sub_ability.is_none(),
            "PutAtLibraryPosition should be the last effect in chain",
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

    // -----------------------------------------------------------------------
    // Channel (CR 207.2c — ability word)
    // -----------------------------------------------------------------------

    #[test]
    fn channel_parses_as_activated_from_hand() {
        // Eiganjo, Seat of the Empire — Channel line
        let r = parse(
            "Channel — {2}{W}, Discard this card: It deals 4 damage to target attacking or blocking creature.",
            "Eiganjo, Seat of the Empire",
            &[],
            &["Land"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        // CR 207.2c: Channel is an ability word — the underlying ability activates from hand
        assert_eq!(ability.activation_zone, Some(Zone::Hand));
        // Cost should contain mana + self-ref discard, not Unimplemented
        match ability.cost.as_ref().unwrap() {
            AbilityCost::Composite { costs } => {
                assert!(
                    costs.iter().any(|c| matches!(c, AbilityCost::Mana { .. })),
                    "Channel cost should include mana, got {:?}",
                    costs
                );
                assert!(
                    costs
                        .iter()
                        .any(|c| matches!(c, AbilityCost::Discard { self_ref: true, .. })),
                    "Channel cost should include self-ref discard, got {:?}",
                    costs
                );
                assert!(
                    !costs
                        .iter()
                        .any(|c| matches!(c, AbilityCost::Unimplemented { .. })),
                    "Channel cost should NOT contain Unimplemented, got {:?}",
                    costs
                );
            }
            other => panic!("Expected Composite cost, got {:?}", other),
        }
        // Effect should not be Unimplemented
        assert!(
            !matches!(*ability.effect, Effect::Unimplemented { .. }),
            "Channel effect should not be Unimplemented, got {:?}",
            ability.effect,
        );
    }

    #[test]
    fn channel_with_em_dash_variant() {
        // Test both em-dash (—) and double-hyphen (--) parsing
        let r = parse(
            "Channel -- {1}{G}, Discard this card: Search your library for a basic land card, reveal it, put it into your hand, then shuffle.",
            "Test Channel Card",
            &[],
            &["Creature"],
            &["Spirit"],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
        assert_eq!(r.abilities[0].activation_zone, Some(Zone::Hand));
    }

    // ── Escape keyword parsing ──────────────────────────────────────────────

    #[test]
    fn parse_escape_sentinels_eyes() {
        // CR 702.138: Standard escape format — {W}, exile two
        let r = parse(
            "Enchant creature\nEnchanted creature gets +1/+1 and has vigilance.\nEscape\u{2014}{W}, Exile two other cards from your graveyard.",
            "Sentinel's Eyes",
            &[Keyword::Enchant(TargetFilter::Typed(crate::types::ability::TypedFilter::creature()))],
            &["Enchantment"],
            &["Aura"],
        );
        let escape_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Escape { .. }));
        assert!(escape_kw.is_some(), "Escape keyword should be extracted");
        match escape_kw.unwrap() {
            Keyword::Escape { cost, exile_count } => {
                assert_eq!(*exile_count, 2);
                assert!(matches!(cost, ManaCost::Cost { generic: 0, shards } if shards.len() == 1));
            }
            _ => unreachable!(),
        }
        // No Unimplemented abilities for the escape line
        assert!(
            !r.abilities
                .iter()
                .any(|a| matches!(*a.effect, Effect::Unimplemented { .. })),
            "Escape line should not produce Unimplemented"
        );
    }

    #[test]
    fn parse_escape_high_cost() {
        // CR 702.138: Higher cost — {3}{B}{B}, exile five
        let r = parse(
            "Escape\u{2014}{3}{B}{B}, Exile five other cards from your graveyard.",
            "Test Card",
            &[],
            &["Creature"],
            &[],
        );
        let escape_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Escape { .. }));
        assert!(escape_kw.is_some());
        match escape_kw.unwrap() {
            Keyword::Escape { cost, exile_count } => {
                assert_eq!(*exile_count, 5);
                assert!(matches!(cost, ManaCost::Cost { generic: 3, shards } if shards.len() == 2));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_escape_eight_exile() {
        // CR 702.138: Edge case — exile eight
        let r = parse(
            "Escape\u{2014}{R}{R}, Exile eight other cards from your graveyard.",
            "Test Card",
            &[],
            &["Creature"],
            &[],
        );
        match r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Escape { .. }))
            .unwrap()
        {
            Keyword::Escape { exile_count, .. } => assert_eq!(*exile_count, 8),
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_harmonize_channeled_dragonfire() {
        // Harmonize — keyword with mana cost parsed from Oracle text.
        // MTGJSON uses space-separated format, NOT em-dash.
        let r = parse(
            "Channeled Dragonfire deals 2 damage to any target.\nHarmonize {5}{R}{R} (You may cast this card from your graveyard for its harmonize cost. You may tap a creature you control to reduce that cost by {X}, where X is its power. Then exile this spell.)",
            "Channeled Dragonfire",
            &[],
            &["Instant"],
            &[],
        );
        let harmonize_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Harmonize(_)));
        assert!(harmonize_kw.is_some(), "Harmonize keyword not extracted");
        match harmonize_kw.unwrap() {
            Keyword::Harmonize(cost) => {
                // {5}{R}{R} = 5 generic + 2 red = total 7
                assert_eq!(cost.mana_value(), 7);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_harmonize_wild_ride() {
        // Harmonize with lower cost
        let r = parse(
            "Target creature gets +3/+0 and gains haste until end of turn.\nHarmonize {4}{R} (You may cast this card from your graveyard for its harmonize cost. You may tap a creature you control to reduce that cost by {X}, where X is its power. Then exile this spell.)",
            "Wild Ride",
            &[],
            &["Instant"],
            &[],
        );
        let harmonize_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Harmonize(_)));
        assert!(harmonize_kw.is_some(), "Harmonize keyword not extracted");
        match harmonize_kw.unwrap() {
            Keyword::Harmonize(cost) => {
                assert_eq!(cost.mana_value(), 5);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_harmonize_no_reminder_text() {
        // Some cards have no reminder text (e.g., Ureni's Counsel)
        let r = parse(
            "Draw three cards.\nHarmonize {8}{R}{R}",
            "Ureni's Counsel",
            &[],
            &["Sorcery"],
            &[],
        );
        let harmonize_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Harmonize(_)));
        assert!(harmonize_kw.is_some(), "Harmonize keyword not extracted");
        match harmonize_kw.unwrap() {
            Keyword::Harmonize(cost) => {
                assert_eq!(cost.mana_value(), 10);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn earthbend_chain_defaults_target() {
        use crate::parser::oracle_effect::parse_effect_chain;

        // Single chunk: "Earthbend 3" — passes through imperative pipeline
        let simple = parse_effect_chain("Earthbend 3", crate::types::ability::AbilityKind::Spell);
        match &*simple.effect {
            Effect::Animate {
                is_earthbend,
                target,
                ..
            } => {
                assert!(is_earthbend);
                assert!(
                    matches!(target, TargetFilter::Typed(tf) if tf.type_filters.contains(&crate::types::ability::TypeFilter::Land)),
                    "simple earthbend should target land, got {target:?}"
                );
            }
            other => panic!("Expected Animate for simple earthbend, got {other:?}"),
        }

        // Full stripped text from Cracked Earth Technique
        let full = parse_effect_chain(
            "Earthbend 3, then earthbend 3. You gain 3 life.",
            crate::types::ability::AbilityKind::Spell,
        );
        eprintln!("Full chain first effect: {:?}", full.effect);
        match &*full.effect {
            Effect::Animate {
                is_earthbend,
                target,
                ..
            } => {
                assert!(is_earthbend, "chain earthbend should be earthbend");
                assert!(
                    matches!(target, TargetFilter::Typed(tf) if tf.type_filters.contains(&crate::types::ability::TypeFilter::Land)),
                    "chain earthbend should target land, got {target:?}"
                );
            }
            other => panic!("Expected Animate for chain earthbend, got {other:?}"),
        }
    }

    #[test]
    fn search_put_onto_battlefield_tapped() {
        use crate::parser::oracle_effect::parse_effect_chain;

        // Rampant Growth pattern: "Search...put that card onto the battlefield tapped, then shuffle."
        let def = parse_effect_chain(
            "Search your library for a basic land card, put that card onto the battlefield tapped, then shuffle.",
            crate::types::ability::AbilityKind::Spell,
        );
        assert!(matches!(&*def.effect, Effect::SearchLibrary { .. }));
        let change_zone = def
            .sub_ability
            .as_ref()
            .expect("should have ChangeZone sub_ability");
        match &*change_zone.effect {
            Effect::ChangeZone {
                origin,
                destination,
                enter_tapped,
                ..
            } => {
                assert_eq!(*origin, Some(crate::types::zones::Zone::Library));
                assert_eq!(*destination, crate::types::zones::Zone::Battlefield);
                assert!(enter_tapped, "searched land should enter tapped");
            }
            other => panic!("Expected ChangeZone, got {other:?}"),
        }
        // "then shuffle" must produce a Shuffle effect in the sub_ability chain
        let shuffle = change_zone
            .sub_ability
            .as_ref()
            .expect("should have Shuffle sub_ability");
        assert!(
            matches!(&*shuffle.effect, Effect::Shuffle { .. }),
            "Expected Shuffle after ChangeZone, got {:?}",
            shuffle.effect,
        );

        // Earthbender pattern: search follows a period + "Then"
        let def2 = parse_effect_chain(
            "Earthbend 2. Then search your library for a basic land card, put it onto the battlefield tapped, then shuffle.",
            crate::types::ability::AbilityKind::Spell,
        );
        // First effect is Animate (earthbend), walk to search via sub_ability
        let search = def2.sub_ability.as_ref().expect("should chain to search");
        assert!(matches!(&*search.effect, Effect::SearchLibrary { .. }));
        let cz = search
            .sub_ability
            .as_ref()
            .expect("should chain to ChangeZone");
        match &*cz.effect {
            Effect::ChangeZone {
                destination,
                enter_tapped,
                ..
            } => {
                assert_eq!(*destination, crate::types::zones::Zone::Battlefield);
                assert!(
                    enter_tapped,
                    "searched land after 'Then' should enter tapped"
                );
            }
            other => panic!("Expected ChangeZone after Then-search, got {other:?}"),
        }
        let shuffle2 = cz
            .sub_ability
            .as_ref()
            .expect("should have Shuffle after earthbender ChangeZone");
        assert!(
            matches!(&*shuffle2.effect, Effect::Shuffle { .. }),
            "Expected Shuffle after earthbender ChangeZone, got {:?}",
            shuffle2.effect,
        );

        // Negative case: search to hand (no "battlefield tapped")
        let tutor = parse_effect_chain(
            "Search your library for a card, put that card into your hand, then shuffle.",
            crate::types::ability::AbilityKind::Spell,
        );
        let cz_hand = tutor.sub_ability.as_ref().expect("should have ChangeZone");
        match &*cz_hand.effect {
            Effect::ChangeZone {
                destination,
                enter_tapped,
                ..
            } => {
                assert_eq!(*destination, crate::types::zones::Zone::Hand);
                assert!(!enter_tapped, "search-to-hand should not be tapped");
            }
            other => panic!("Expected ChangeZone to Hand, got {other:?}"),
        }
        let shuffle3 = cz_hand
            .sub_ability
            .as_ref()
            .expect("should have Shuffle after search-to-hand");
        assert!(
            matches!(&*shuffle3.effect, Effect::Shuffle { .. }),
            "Expected Shuffle after search-to-hand ChangeZone, got {:?}",
            shuffle3.effect,
        );
    }

    #[test]
    fn strip_counter_conditional_prefix_quest() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, AbilityKind, Comparator, QuantityExpr, QuantityRef,
        };

        let def = parse_effect_chain(
            "if it has four or more quest counters on it, put a +1/+1 counter on target creature you control",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &def.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOnSelf { counter_type } },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 4 },
                }) if counter_type == "quest"
            ),
            "Expected QuantityCheck(quest >= 4), got {:?}",
            def.condition,
        );
        assert!(
            matches!(&*def.effect, Effect::PutCounter { counter_type, count: QuantityExpr::Fixed { value: 1 }, .. } if counter_type == "P1P1"),
            "Expected PutCounter P1P1, got {:?}",
            def.effect,
        );
    }

    #[test]
    fn strip_counter_conditional_suffix_hunger() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, AbilityKind, Comparator, QuantityExpr, QuantityRef,
        };

        let def = parse_effect_chain(
            "destroy this enchantment if it has five or more hunger counters on it",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &def.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOnSelf { counter_type } },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 5 },
                }) if counter_type == "hunger"
            ),
            "Expected QuantityCheck(hunger >= 5), got {:?}",
            def.condition,
        );
        assert!(
            matches!(&*def.effect, Effect::Destroy { .. }),
            "Expected Destroy effect, got {:?}",
            def.effect,
        );
    }

    #[test]
    fn strip_counter_conditional_p1p1_normalization() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, AbilityKind, Comparator, QuantityExpr, QuantityRef,
        };

        let def = parse_effect_chain(
            "if it has three or more +1/+1 counters on it, sacrifice this Aura",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &def.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOnSelf { counter_type } },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                }) if counter_type == "P1P1"
            ),
            "Expected QuantityCheck(P1P1 >= 3), got {:?}",
            def.condition,
        );
    }

    #[test]
    fn strip_counter_conditional_one_or_more_oil() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, AbilityKind, Comparator, QuantityExpr, QuantityRef,
        };

        let def = parse_effect_chain(
            "if it has one or more oil counters on it, put an oil counter on it",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &def.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOnSelf { counter_type } },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                }) if counter_type == "oil"
            ),
            "Expected QuantityCheck(oil >= 1), got {:?}",
            def.condition,
        );
    }

    #[test]
    fn strip_counter_conditional_no_ice_counters() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, AbilityKind, Comparator, QuantityExpr, QuantityRef,
        };

        let def = parse_effect_chain(
            "if it has no ice counters on it, transform it",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &def.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOnSelf { counter_type } },
                    comparator: Comparator::EQ,
                    rhs: QuantityExpr::Fixed { value: 0 },
                }) if counter_type == "ice"
            ),
            "Expected QuantityCheck(ice == 0), got {:?}",
            def.condition,
        );
    }

    #[test]
    fn earthbender_ascension_landfall_chain() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{
            AbilityCondition, AbilityKind, Comparator, QuantityExpr, QuantityRef, TargetFilter,
        };

        let def = parse_effect_chain(
            "put a quest counter on this enchantment. When you do, if it has four or more quest counters on it, put a +1/+1 counter on target creature you control. It gains trample until end of turn.",
            AbilityKind::Spell,
        );

        // Node 1: PutCounter(quest, 1, SelfRef), no condition
        assert!(def.condition.is_none(), "Node 1 should have no condition");
        assert!(
            matches!(&*def.effect, Effect::PutCounter { counter_type, count: QuantityExpr::Fixed { value: 1 }, target: TargetFilter::SelfRef } if counter_type == "quest"),
            "Node 1 should be PutCounter(quest, SelfRef), got {:?}",
            def.effect,
        );

        // Node 2: PutCounter(P1P1, 1, Typed(creature+You)), condition = QuantityCheck(quest >= 4)
        let node2 = def
            .sub_ability
            .as_ref()
            .expect("should have node 2 (P1P1 counter)");
        assert!(
            matches!(
                &node2.condition,
                Some(AbilityCondition::QuantityCheck {
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOnSelf { counter_type } },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 4 },
                }) if counter_type == "quest"
            ),
            "Node 2 condition should be QuantityCheck(quest >= 4), got {:?}",
            node2.condition,
        );
        match &*node2.effect {
            Effect::PutCounter {
                counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(tf),
            } => {
                assert_eq!(counter_type, "P1P1");
                assert!(
                    tf.controller == Some(crate::types::ability::ControllerRef::You),
                    "P1P1 target should be creature you control, got {:?}",
                    tf,
                );
            }
            other => panic!("Node 2 should be PutCounter(P1P1, Typed), got {other:?}"),
        }

        // Node 3: GenericEffect(trample, ParentTarget), duration = UntilEndOfTurn
        let node3 = node2
            .sub_ability
            .as_ref()
            .expect("should have node 3 (trample grant)");
        match &*node3.effect {
            Effect::GenericEffect {
                target, duration, ..
            } => {
                assert!(
                    matches!(target, Some(TargetFilter::ParentTarget)),
                    "Node 3 target should be ParentTarget, got {target:?}",
                );
                assert!(
                    matches!(
                        duration,
                        Some(crate::types::ability::Duration::UntilEndOfTurn)
                    ),
                    "Node 3 duration should be UntilEndOfTurn, got {duration:?}",
                );
            }
            other => panic!("Node 3 should be GenericEffect(trample), got {other:?}"),
        }
    }

    #[test]
    fn semicolon_keyword_splitting_defender_reach() {
        let r = parse_with_keyword_names(
            "Defender; reach",
            "Wall of Nets",
            &["defender", "reach"],
            &["Creature"],
            &["Wall"],
        );
        assert!(
            r.extracted_keywords.is_empty(),
            "MTGJSON-covered keywords should not be re-extracted"
        );
        // The key assertion: both keywords are recognized (no unimplemented abilities)
        assert!(
            r.abilities.is_empty(),
            "No abilities should be produced from a keyword-only line"
        );
    }

    #[test]
    fn semicolon_keyword_splitting_first_strike_banding() {
        let r = parse_with_keyword_names(
            "First strike; banding",
            "Test Card",
            &["first strike", "banding"],
            &["Creature"],
            &[],
        );
        assert!(
            r.abilities.is_empty(),
            "No abilities from keyword-only semicolon line"
        );
    }

    #[test]
    fn semicolon_keyword_splitting_vigilance_menace() {
        let r = parse_with_keyword_names(
            "Vigilance; menace",
            "Test Card",
            &["vigilance", "menace"],
            &["Creature"],
            &[],
        );
        assert!(
            r.abilities.is_empty(),
            "No abilities from keyword-only semicolon line"
        );
    }

    #[test]
    fn semicolon_does_not_split_activated_ability() {
        // A line with a colon should NOT be split on semicolons
        let r = parse_with_keyword_names(
            "{T}: Draw a card; you lose 1 life.",
            "Test Card",
            &[],
            &["Creature"],
            &[],
        );
        // Should be parsed as a single activated ability
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
    }

    #[test]
    fn semicolon_no_split_single_keyword() {
        // A single keyword without semicolons should continue to work
        let r =
            parse_with_keyword_names("Flying", "Test Bird", &["flying"], &["Creature"], &["Bird"]);
        assert!(
            r.abilities.is_empty(),
            "No abilities from single keyword line"
        );
    }

    // -- Strive parsing tests --------------------------------------------------

    #[test]
    fn strive_mana_symbol_parse() {
        use crate::parser::oracle_util::parse_mana_symbols;
        let result = parse_mana_symbols("{2}{U}");
        assert!(result.is_some());
        let (cost, rest) = result.unwrap();
        assert_eq!(cost.mana_value(), 3);
        assert_eq!(rest, "");
    }

    #[test]
    fn strive_ability_word_strip() {
        use crate::parser::oracle_modal::strip_ability_word;
        let input = "Strive \u{2014} This spell costs {2}{U} more to cast for each target beyond the first.";
        let stripped = strip_ability_word(input);
        assert!(
            stripped.is_some(),
            "strip_ability_word should match Strive line"
        );
        let text = stripped.unwrap();
        assert!(
            text.starts_with("This spell costs"),
            "expected 'This spell costs...' got: {}",
            text
        );
    }

    #[test]
    fn strive_cost_parsed_from_oracle_text() {
        // CR 207.2c + CR 601.2f: Strive per-target surcharge.
        let text = "Strive \u{2014} This spell costs {2}{U} more to cast for each target beyond the first.";
        let r = parse(text, "Test Card", &[], &["Instant"], &[]);
        assert!(r.strive_cost.is_some());
        assert_eq!(r.strive_cost.unwrap().mana_value(), 3);
    }

    #[test]
    fn strive_cost_parsed_different_cost() {
        let r = parse(
            "Strive — This spell costs {1}{B} more to cast for each target beyond the first.\nDestroy target creature.",
            "Cruel Feeding",
            &[],
            &["Instant"],
            &[],
        );
        assert!(r.strive_cost.is_some(), "strive_cost should be parsed");
        let cost = r.strive_cost.unwrap();
        assert_eq!(cost.mana_value(), 2);
    }

    #[test]
    fn no_strive_cost_on_normal_spell() {
        let r = parse(
            "Target creature gets +3/+3 until end of turn.",
            "Giant Growth",
            &[],
            &["Instant"],
            &[],
        );
        assert!(r.strive_cost.is_none());
    }

    #[test]
    fn strive_line_consumed_not_reparsed() {
        let r = parse(
            "Strive \u{2014} This spell costs {1}{R} more to cast for each target beyond the first.\nDraw a card.",
            "Test Strive Card",
            &[],
            &["Instant"],
            &[],
        );
        assert!(r.strive_cost.is_some());
        assert!(
            r.abilities.len() <= 2,
            "strive_cost was set; abilities={}",
            r.abilities.len()
        );
        let has_strive_ability = r.abilities.iter().any(|a| {
            a.description
                .as_ref()
                .is_some_and(|d| d.to_lowercase().contains("strive"))
        });
        assert!(
            !has_strive_ability,
            "strive line should be consumed, not produce an ability"
        );
    }

    // ── Mana spend restriction extensions ─────────────────────────────

    #[test]
    fn mana_spend_restriction_activate_only() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result = parse_mana_spend_restriction("spend this mana only to activate abilities");
        assert_eq!(result, Some(ManaSpendRestriction::ActivateOnly));
    }

    #[test]
    fn mana_spend_restriction_noncreature_spells() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result =
            parse_mana_spend_restriction("spend this mana only to cast noncreature spells");
        assert_eq!(
            result,
            Some(ManaSpendRestriction::SpellType("Noncreature".to_string()))
        );
    }

    #[test]
    fn mana_spend_restriction_x_cost_only() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result = parse_mana_spend_restriction("spend this mana only on costs that include {x}");
        assert_eq!(result, Some(ManaSpendRestriction::XCostOnly));
    }

    #[test]
    fn mana_spend_restriction_instant_or_sorcery() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result =
            parse_mana_spend_restriction("spend this mana only to cast instant or sorcery spells");
        assert_eq!(
            result,
            Some(ManaSpendRestriction::SpellType(
                "Instant or Sorcery".to_string()
            ))
        );
    }

    // ── Each player/opponent iteration ────────────────────────────────

    #[test]
    fn each_opponent_discards_produces_player_scope() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::PlayerFilter;
        let def = parse_effect_chain(
            "each opponent discards a card",
            crate::types::ability::AbilityKind::Spell,
        );
        assert_eq!(
            def.player_scope,
            Some(PlayerFilter::Opponent),
            "player_scope should be Opponent for 'each opponent discards'"
        );
        assert!(
            matches!(*def.effect, Effect::Discard { .. }),
            "inner effect should be Discard, got {:?}",
            def.effect,
        );
    }

    #[test]
    fn each_player_draws_produces_player_scope() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::PlayerFilter;
        let def = parse_effect_chain(
            "each player draws a card",
            crate::types::ability::AbilityKind::Spell,
        );
        assert_eq!(
            def.player_scope,
            Some(PlayerFilter::All),
            "player_scope should be All for 'each player draws'"
        );
        assert!(
            matches!(*def.effect, Effect::Draw { .. }),
            "inner effect should be Draw, got {:?}",
            def.effect,
        );
    }

    #[test]
    fn each_opponent_loses_life_produces_player_scope() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::PlayerFilter;
        let def = parse_effect_chain(
            "each opponent loses 2 life",
            crate::types::ability::AbilityKind::Spell,
        );
        assert_eq!(
            def.player_scope,
            Some(PlayerFilter::Opponent),
            "player_scope should be Opponent for 'each opponent loses 2 life'"
        );
        assert!(
            matches!(*def.effect, Effect::LoseLife { .. }),
            "inner effect should be LoseLife, got {:?}",
            def.effect,
        );
    }

    #[test]
    fn each_opponent_mills_produces_player_scope() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::PlayerFilter;
        let def = parse_effect_chain(
            "each opponent mills three cards",
            crate::types::ability::AbilityKind::Spell,
        );
        assert_eq!(
            def.player_scope,
            Some(PlayerFilter::Opponent),
            "player_scope should be Opponent for 'each opponent mills'"
        );
        assert!(
            matches!(*def.effect, Effect::Mill { .. }),
            "inner effect should be Mill, got {:?}",
            def.effect,
        );
    }

    // --- Static parser greediness: spell lines with damage + restriction ---

    #[test]
    fn spell_damage_plus_cant_block_not_static() {
        // Mugging: "deals 2 damage to target creature. That creature can't block this turn."
        // Must produce a spell ability with DealDamage, NOT a static CantBlock.
        let r = parse(
            "Mugging deals 2 damage to target creature. That creature can't block this turn.",
            "Mugging",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(
            r.statics.is_empty(),
            "spell damage line should not produce static, got {:?}",
            r.statics
        );
        assert_eq!(r.abilities.len(), 1, "should produce one spell ability");
        assert!(
            matches!(*r.abilities[0].effect, Effect::DealDamage { .. }),
            "first effect should be DealDamage, got {:?}",
            r.abilities[0].effect
        );
        assert!(
            r.abilities[0].sub_ability.is_some(),
            "should chain to restriction sub_ability"
        );
    }

    #[test]
    fn spell_restriction_then_damage_skullcrack() {
        // Skullcrack: "Players can't gain life this turn. Damage can't be prevented this turn.
        //              Skullcrack deals 3 damage to target player or planeswalker."
        let r = parse(
            "Players can't gain life this turn. Damage can't be prevented this turn. Skullcrack deals 3 damage to target player or planeswalker.",
            "Skullcrack",
            &[],
            &["Instant"],
            &[],
        );
        assert!(
            r.statics.is_empty(),
            "spell damage line should not produce static, got {:?}",
            r.statics
        );
        assert_eq!(r.abilities.len(), 1);
        // Chain: GenericEffect(CantGainLife) → AddRestriction → DealDamage
        let ab = &r.abilities[0];
        assert!(
            matches!(*ab.effect, Effect::GenericEffect { .. }),
            "first clause should be GenericEffect(CantGainLife), got {:?}",
            ab.effect
        );
        let sub1 = ab
            .sub_ability
            .as_ref()
            .expect("should chain to AddRestriction");
        assert!(
            matches!(*sub1.effect, Effect::AddRestriction { .. }),
            "second clause should be AddRestriction, got {:?}",
            sub1.effect
        );
        let sub2 = sub1
            .sub_ability
            .as_ref()
            .expect("should chain to DealDamage");
        assert!(
            matches!(*sub2.effect, Effect::DealDamage { .. }),
            "third clause should be DealDamage, got {:?}",
            sub2.effect
        );
    }

    #[test]
    fn avatars_wrath_parses_airbend_chain_cast_restriction_and_self_exile() {
        let r = parse(
            "Choose up to one target creature, then airbend all other creatures. (Exile them. While each one is exiled, its owner may cast it for {2} rather than its mana cost.)\nUntil your next turn, your opponents can't cast spells from anywhere other than their hands.\nExile Avatar's Wrath.",
            "Avatar's Wrath",
            &[],
            &["Sorcery"],
            &[],
        );

        assert_eq!(r.abilities.len(), 3);
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::TargetOnly {
                target: TargetFilter::Typed(_),
            }
        ));
        let airbend = r.abilities[0]
            .sub_ability
            .as_ref()
            .expect("airbend clause should chain from TargetOnly");
        assert!(matches!(
            *airbend.effect,
            Effect::ChangeZoneAll {
                destination: Zone::Exile,
                ..
            }
        ));
        let permission = airbend
            .sub_ability
            .as_ref()
            .expect("airbend clause should grant exile-cast permission");
        assert!(matches!(
            *permission.effect,
            Effect::GrantCastingPermission { .. }
        ));

        assert!(matches!(
            *r.abilities[1].effect,
            Effect::AddRestriction {
                restriction: crate::types::ability::GameRestriction::CastOnlyFromZones { .. }
            }
        ));
        assert_eq!(
            r.abilities[1].duration,
            Some(crate::types::ability::Duration::UntilYourNextTurn)
        );

        assert!(matches!(
            *r.abilities[2].effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    #[test]
    fn spell_damage_plus_doesnt_untap() {
        // Chandra's Revolution: "deals 4 damage to target creature. Tap target land.
        //                        That land doesn't untap during its controller's next untap step."
        let r = parse(
            "Chandra's Revolution deals 4 damage to target creature. Tap target land. That land doesn't untap during its controller's next untap step.",
            "Chandra's Revolution",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(
            r.statics.is_empty(),
            "spell damage line should not produce static, got {:?}",
            r.statics
        );
        assert!(!r.abilities.is_empty(), "should produce spell abilities");
        assert!(
            matches!(*r.abilities[0].effect, Effect::DealDamage { .. }),
            "first effect should be DealDamage, got {:?}",
            r.abilities[0].effect
        );
    }

    #[test]
    fn creature_cant_block_still_produces_static() {
        // Regression guard: non-spell "can't block" must still produce static.
        let r = parse(
            "Defender\nThis creature can't attack.",
            "Guard Gomazoa",
            &[Keyword::Defender],
            &["Creature"],
            &[],
        );
        assert!(
            !r.statics.is_empty(),
            "creature restriction should still produce static"
        );
    }

    #[test]
    fn spell_pump_all_with_duration_not_static() {
        // CR 611.2a: Spell lines with subject + pump + duration are one-shot
        // continuous effects, not permanent static abilities.
        let r = parse(
            "Creatures you control get +2/+0 until end of turn.",
            "Test Spell",
            &[],
            &["Instant"],
            &[],
        );
        assert!(
            r.statics.is_empty(),
            "spell pump-all with duration should not produce static, got {:?}",
            r.statics,
        );
        assert_eq!(r.abilities.len(), 1, "should produce one spell ability");
        assert!(
            matches!(*r.abilities[0].effect, Effect::PumpAll { .. }),
            "effect should be PumpAll, got {:?}",
            r.abilities[0].effect,
        );
    }

    #[test]
    fn permanent_pump_all_without_duration_stays_static() {
        // CR 611.3a: Same pattern on a permanent is a static ability.
        let r = parse(
            "Creatures you control get +1/+1.",
            "Test Enchantment",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(
            !r.statics.is_empty(),
            "permanent pump-all should produce static ability",
        );
        assert!(
            r.abilities.is_empty(),
            "permanent pump-all should not produce spell ability, got {:?}",
            r.abilities,
        );
    }

    #[test]
    fn spell_restriction_with_duration_not_static() {
        // CR 611.2a: Spell lines with a restriction + duration are one-shot
        // continuous effects, not permanent statics. Tests a non-pump
        // `is_static_pattern` variant ("can't block") with a duration marker.
        let r = parse(
            "Creatures your opponents control can't block this turn.",
            "Test Spell",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(
            r.statics.is_empty(),
            "spell restriction with duration should not produce static, got {:?}",
            r.statics,
        );
        assert_eq!(r.abilities.len(), 1, "should produce one spell ability");
    }

    #[test]
    fn multi_line_spell_preserves_non_damage_static() {
        // Line 1 (no damage) should produce static; line 2 (damage) should produce ability.
        let r = parse(
            "Creatures you control have haste.\nBarrage of Boulders deals 1 damage to each creature you don't control.",
            "Barrage of Boulders",
            &[],
            &["Sorcery"],
            &[],
        );
        assert!(
            !r.statics.is_empty(),
            "non-damage line should still produce static"
        );
        assert!(
            !r.abilities.is_empty(),
            "damage line should produce spell ability"
        );
    }

    #[test]
    fn collected_company_dig_from_among() {
        let r = parse(
            "Look at the top six cards of your library. Put up to two creature cards with mana value 3 or less from among them onto the battlefield. Put the rest on the bottom of your library in any order.",
            "Collected Company",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1, "should produce one ability");
        match &*r.abilities[0].effect {
            Effect::Dig {
                count,
                destination,
                keep_count,
                up_to,
                filter,
                rest_destination,
            } => {
                assert_eq!(
                    *count,
                    QuantityExpr::Fixed { value: 6 },
                    "dig count should be 6"
                );
                assert_eq!(
                    *destination,
                    Some(Zone::Battlefield),
                    "kept cards go to battlefield"
                );
                assert_eq!(*keep_count, Some(2), "keep up to 2");
                assert!(*up_to, "should be up_to");
                assert!(
                    matches!(filter, TargetFilter::Typed(TypedFilter { ref type_filters, .. })
                        if type_filters.contains(&TypeFilter::Creature)),
                    "filter should require creatures, got {:?}",
                    filter,
                );
                assert_eq!(
                    *rest_destination,
                    Some(Zone::Library),
                    "rest go to bottom of library"
                );
            }
            other => {
                panic!(
                    "Expected Dig effect, got {:?}",
                    std::mem::discriminant(other)
                );
            }
        }
    }

    #[test]
    fn commune_with_nature_dig_from_among() {
        let r = parse(
            "Look at the top five cards of your library. You may reveal a creature card from among them and put it into your hand. Put the rest on the bottom of your library in any order.",
            "Commune with Nature",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        match &*r.abilities[0].effect {
            Effect::Dig {
                count,
                destination,
                keep_count,
                up_to,
                filter,
                rest_destination,
            } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 5 });
                assert_eq!(*destination, Some(Zone::Hand));
                assert_eq!(*keep_count, Some(1));
                assert!(*up_to, "a creature card = up to 1");
                assert!(
                    matches!(filter, TargetFilter::Typed(TypedFilter { ref type_filters, .. })
                        if type_filters.contains(&TypeFilter::Creature)),
                    "filter should require creatures",
                );
                assert_eq!(*rest_destination, Some(Zone::Library));
            }
            other => {
                panic!(
                    "Expected Dig effect, got {:?}",
                    std::mem::discriminant(other)
                );
            }
        }
    }

    #[test]
    fn heroic_trigger_not_misrouted_to_replacement() {
        // Favored Hoplite: "Heroic — Whenever you cast a spell that targets this creature,
        // put a +1/+1 counter on this creature and prevent all damage that would be dealt
        // to it this turn."
        // Should produce a trigger, NOT a replacement.
        let result = parse(
            "Heroic — Whenever you cast a spell that targets this creature, put a +1/+1 counter on this creature and prevent all damage that would be dealt to it this turn.",
            "Favored Hoplite",
            &[],
            &["Creature"],
            &["Human", "Soldier"],
        );
        assert_eq!(
            result.triggers.len(),
            1,
            "Should have 1 trigger, got {} triggers and {} replacements. triggers={:?} replacements={:?}",
            result.triggers.len(),
            result.replacements.len(),
            result.triggers,
            result.replacements,
        );
        assert_eq!(
            result.replacements.len(),
            0,
            "Should have 0 replacements, got {}: {:?}",
            result.replacements.len(),
            result.replacements,
        );
    }

    #[test]
    fn ability_word_trigger_not_static_or_replacement() {
        // "Constellation — Whenever an enchantment enters the battlefield under your control,
        // you gain 1 life." — ability-word-prefixed trigger should route to triggers.
        let result = parse(
            "Constellation — Whenever an enchantment you control enters, you gain 1 life.",
            "Test Card",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(
            result.triggers.len(),
            1,
            "Ability-word trigger should produce 1 trigger, got: triggers={:?}",
            result.triggers,
        );
    }

    #[test]
    fn b20_platinum_angel_both_statics() {
        // B20: Compound "can't win/lose" line must emit BOTH statics
        let result = parse(
            "You can't lose the game and your opponents can't win the game.",
            "Platinum Angel",
            &[],
            &["Creature"],
            &[],
        );
        assert!(
            result
                .statics
                .iter()
                .any(|s| s.mode == StaticMode::CantLoseTheGame),
            "should emit CantLoseTheGame, got: {:?}",
            result.statics,
        );
        assert!(
            result
                .statics
                .iter()
                .any(|s| s.mode == StaticMode::CantWinTheGame),
            "should emit CantWinTheGame, got: {:?}",
            result.statics,
        );
    }

    #[test]
    fn discard_unless_creature_card() {
        let r = parse(
            "Draw three cards. Then discard two cards unless you discard a creature card.",
            "Winternight Stories",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let sub = r.abilities[0]
            .sub_ability
            .as_ref()
            .expect("Should have sub_ability for discard");
        match &*sub.effect {
            Effect::Discard {
                count,
                unless_filter,
                ..
            } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
                assert!(unless_filter.is_some(), "Expected unless_filter, got None");
            }
            other => panic!("Expected Discard, got {:?}", std::mem::discriminant(other)),
        }
    }
}
