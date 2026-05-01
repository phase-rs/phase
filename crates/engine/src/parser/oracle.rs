use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::{all_consuming, opt, value};
use nom::Parser;
use nom_language::error::VerboseError;
use serde::{Deserialize, Serialize};

use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction,
    AdditionalCost, CastingRestriction, Comparator, ContinuousModification, Effect, ModalChoice,
    ReplacementDefinition, SolveCondition, SpellCastingOption, StaticCondition, StaticDefinition,
    TargetFilter, TriggerCondition, TriggerDefinition, TypedFilter,
};
use crate::types::keywords::{FlashbackCost, Keyword, KeywordKind};
use crate::types::mana::ManaCost;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

use super::oracle_nom::bridge::{nom_on_lower, split_once_on_lower};
use super::oracle_nom::condition::parse_inner_condition;
use super::oracle_nom::primitives::scan_contains;
use super::oracle_warnings::{clear_warnings, push_warning, take_warnings};

use super::oracle_casting::{
    parse_additional_cost_line, parse_casting_restriction_line, parse_spell_casting_option_line,
};
use super::oracle_class::parse_class_oracle_text;
use super::oracle_classifier::{
    has_roll_die_pattern, has_trigger_prefix, is_ability_activate_cost_static,
    is_cant_win_lose_compound, is_compound_turn_limit, is_defiler_cost_pattern,
    is_flashback_equal_mana_cost, is_granted_static_line, is_instead_replacement_line,
    is_opening_hand_begin_game, is_replacement_pattern, is_static_pattern, is_vehicle_tier_line,
    lower_starts_with, should_defer_spell_to_effect,
};
use super::oracle_condition::parse_restriction_condition;
use super::oracle_cost::{parse_oracle_cost, try_parse_cost_reduction};
use super::oracle_dispatch::{dispatch_line_nom, make_unimplemented_with_effect};
use super::oracle_effect::{parse_effect_chain, parse_effect_chain_with_context, ParseContext};
pub use super::oracle_keyword::keyword_display_name;
use super::oracle_keyword::{
    extract_keyword_line, is_keyword_cost_line, parse_keyword_from_oracle,
    parse_kicker_additional_cost_line,
};
use super::oracle_level::parse_level_blocks;
use super::oracle_modal::{
    extract_ability_word_reminder_body, lower_oracle_block, parse_oracle_block, strip_ability_word,
    strip_ability_word_with_name,
};
use super::oracle_replacement::parse_replacement_line;
use super::oracle_saga::{is_saga_chapter, parse_saga_chapters};
use super::oracle_spacecraft::parse_spacecraft_threshold_lines;
use super::oracle_special::{
    attach_die_result_branches_to_chain, normalize_self_refs_for_static,
    parse_cumulative_upkeep_keyword, parse_defiler_cost_reduction, parse_escape_keyword,
    parse_harmonize_keyword, parse_solve_condition, try_parse_die_roll_table,
};
use super::oracle_static::{
    parse_static_line_multi, try_parse_graveyard_keyword_grant_clause, GraveyardGrantedKeywordKind,
};
use super::oracle_trigger::parse_trigger_lines_at_index;
use super::oracle_util::{
    normalize_card_name_refs, parse_mana_symbols, parse_number, strip_reminder_text, TextPair,
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
    /// Diagnostic warnings from silent fallback patterns during parsing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parse_warnings: Vec<String>,
}

fn merge_kicker_additional_cost(slot: &mut Option<AdditionalCost>, incoming: AdditionalCost) {
    match incoming {
        AdditionalCost::Kicker {
            costs: incoming_costs,
            repeatable: false,
        } => {
            if let Some(AdditionalCost::Kicker {
                costs,
                repeatable: false,
            }) = slot.as_mut()
            {
                costs.extend(incoming_costs);
            } else {
                *slot = Some(AdditionalCost::Kicker {
                    costs: incoming_costs,
                    repeatable: false,
                });
            }
        }
        incoming => *slot = Some(incoming),
    }
}

fn definition_grants_flashback(def: &AbilityDefinition) -> bool {
    let grants_here = match &*def.effect {
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities.iter().any(|static_def| {
            static_def.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    crate::types::ability::ContinuousModification::AddKeyword { keyword }
                        if keyword.kind() == KeywordKind::Flashback
                )
            })
        }),
        _ => false,
    };

    grants_here
        || def
            .sub_ability
            .as_deref()
            .is_some_and(definition_grants_flashback)
}

fn parse_commander_permission_sentence(input: &str) -> nom::IResult<&str, (), VerboseError<&str>> {
    let (input, subject) = take_until(" can be your commander").parse(input)?;
    if subject.trim().is_empty() {
        return Err(nom::Err::Error(VerboseError {
            errors: vec![(
                input,
                nom_language::error::VerboseErrorKind::Nom(nom::error::ErrorKind::TakeUntil),
            )],
        }));
    }
    let (input, _) = tag(" can be your commander").parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    Ok((input, ()))
}

/// Deck-construction permission text has no runtime ability to resolve.
pub(crate) fn is_commander_permission_sentence(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    let parsed = all_consuming(parse_commander_permission_sentence)
        .parse(lower.as_str())
        .is_ok();
    parsed
}

/// Whether Oracle text explicitly permits this card to be a commander.
pub fn oracle_text_allows_commander(oracle_text: &str, card_name: &str) -> bool {
    let normalized = normalize_card_name_refs(oracle_text, card_name);
    normalized.lines().any(is_commander_permission_sentence)
        || scan_contains(&oracle_text.to_ascii_lowercase(), "can be your commander")
}

fn parsed_result_recently_granted_flashback(result: &ParsedAbilities) -> bool {
    result
        .abilities
        .last()
        .is_some_and(definition_grants_flashback)
        || result.triggers.last().is_some_and(|trigger| {
            trigger
                .execute
                .as_deref()
                .is_some_and(definition_grants_flashback)
        })
        || result.statics.last().is_some_and(|static_def| {
            static_def.modifications.iter().any(|modification| {
                matches!(
                    modification,
                    crate::types::ability::ContinuousModification::AddKeyword { keyword }
                        if keyword.kind() == KeywordKind::Flashback
                )
            })
        })
}

fn parse_graveyard_keyword_continuation(
    text: &str,
    kind: GraveyardGrantedKeywordKind,
) -> Option<Keyword> {
    fn continuation_fully_consumed(rest: &str) -> bool {
        rest.trim().trim_end_matches('.').trim().is_empty()
    }

    let lower = text.to_lowercase();

    match kind {
        GraveyardGrantedKeywordKind::Flashback => {
            let (_, rest) = nom_on_lower(text, &lower, |i| {
                value((), tag("the flashback cost is equal to ")).parse(i)
            })?;
            let rest_lower = rest.to_lowercase();
            let (_, rest) = nom_on_lower(rest, &rest_lower, |i| {
                value(
                    (),
                    alt((
                        tag("that card's mana cost"),
                        tag("the card's mana cost"),
                        tag("its mana cost"),
                    )),
                )
                .parse(i)
            })?;
            if !continuation_fully_consumed(rest) {
                return None;
            }
            Some(Keyword::Flashback(FlashbackCost::Mana(
                ManaCost::SelfManaCost,
            )))
        }
        GraveyardGrantedKeywordKind::Escape => {
            let (_, rest) = nom_on_lower(text, &lower, |i| {
                value((), tag("the escape cost is equal to ")).parse(i)
            })?;
            let rest_lower = rest.to_lowercase();
            let (_, rest) = nom_on_lower(rest, &rest_lower, |i| {
                value(
                    (),
                    alt((
                        tag("that card's mana cost plus exile "),
                        tag("the card's mana cost plus exile "),
                        tag("its mana cost plus exile "),
                    )),
                )
                .parse(i)
            })?;
            let (exile_count, rest) = parse_number(rest)?;
            let rest_lower = rest.to_lowercase();
            let (_, rest) = nom_on_lower(rest, &rest_lower, |i| {
                value((), tag("other cards from your graveyard")).parse(i)
            })?;
            if !continuation_fully_consumed(rest) {
                return None;
            }
            Some(Keyword::Escape {
                cost: ManaCost::SelfManaCost,
                exile_count,
            })
        }
    }
}

fn try_parse_graveyard_keyword_static_with_continuation(line: &str) -> Option<StaticDefinition> {
    let lower = line.to_lowercase();
    let (prefix, continuation) = split_once_on_lower(line, &lower, ". ")?;
    let (affected, kind) = try_parse_graveyard_keyword_grant_clause(prefix)?;
    let keyword = parse_graveyard_keyword_continuation(continuation, kind)?;
    kind.matches_keyword(&keyword).then_some(
        StaticDefinition::continuous()
            .affected(affected)
            .modifications(vec![ContinuousModification::AddKeyword { keyword }])
            .description(line.to_string()),
    )
}

/// Returns every `StaticDefinition` produced by `line`, with the
/// graveyard-keyword-continuation front door checked first (CR 702.99 etc.)
/// and then delegating to `parse_static_line_multi` so compound forms
/// (e.g., cross-mode conjunctions) emit all their constituent statics
/// rather than silently dropping the extras.
fn parse_static_line_with_graveyard_keyword_continuation(line: &str) -> Vec<StaticDefinition> {
    if let Some(def) = try_parse_graveyard_keyword_static_with_continuation(line) {
        return vec![def];
    }
    parse_static_line_multi(line)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ActivatedConstraintAst {
    pub(super) restrictions: Vec<ActivationRestriction>,
    /// CR 602.2: "Any player may activate this ability." — annotation recognized
    /// during parsing. Runtime enforcement is a future item; currently stripped
    /// so the sentence does not produce an `Unimplemented` fallback.
    pub(super) any_player_may_activate: bool,
}

impl ActivatedConstraintAst {
    pub(super) fn sorcery_speed(&self) -> bool {
        self.restrictions
            .contains(&ActivationRestriction::AsSorcery)
    }
}

/// CR 608.2c: Pre-strip "instead if [condition]" or trailing "instead" from effect text.
/// The "instead" keyword signals a cross-line replacement pattern. The trailing
/// "if [condition]" (when present after "instead") is parsed through the shared
/// condition grammar and composed with any ability-word condition at the caller.
fn strip_instead_suffix(text: &str) -> (String, Option<AbilityCondition>, bool) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Pattern: " instead if [condition]" — mid-line "instead" followed by condition
    if let Some((before, after)) = tp.rsplit_around(" instead if ") {
        let condition_text = after.lower.trim().trim_end_matches('.');
        let condition = parse_inner_condition(condition_text)
            .ok()
            .and_then(|(rest, condition)| rest.trim().is_empty().then_some(condition))
            .and_then(|condition| ability_word_to_ability_condition(&Some(condition)));
        return (before.original.trim().to_string(), condition, true);
    }

    // Pattern: "[effect] instead" — trailing "instead" (with optional period)
    if let Some((before, after)) = tp.rsplit_around(" instead") {
        // Guard: "instead" must be at end of text (not "instead of" compound)
        let remainder = after.lower.trim().trim_end_matches('.');
        if remainder.is_empty() {
            // CR 608.2c guard: Only treat as a cross-line "instead" replacement when
            // the "instead" clause covers the whole effect line (i.e., the remaining
            // text is a single conditional sentence). When there is a prior sentence
            // in the same line (Rite of Replication, Saproling Migration: "Create X.
            // If kicked, create Y instead."), the "instead" is an intra-chain override
            // and must be handled by `strip_additional_cost_conditional` inside the
            // chain parser to produce `AdditionalCostPaidInstead` on the sub-ability.
            let before_trim = before.original.trim().trim_end_matches('.');
            if !before_trim.contains('.') {
                return (before.original.trim().to_string(), None, true);
            }
        }
    }

    (text.to_string(), None, false)
}

#[derive(Debug, Clone)]
struct SpellResolutionLine {
    line: String,
    effect_text: String,
    ability_word_condition: Option<StaticCondition>,
    has_ability_word_prefix: bool,
}

fn prepare_spell_resolution_line(raw_line: &str) -> Option<SpellResolutionLine> {
    let raw_line = raw_line.trim();
    if raw_line.is_empty() {
        return None;
    }

    let reminder_body_owned = extract_ability_word_reminder_body(raw_line);
    let raw_line = reminder_body_owned.as_deref().unwrap_or(raw_line);
    let line = strip_x_cant_be_zero_suffix(&strip_reminder_text(raw_line));
    if line.is_empty() {
        return None;
    }

    let (ability_word_condition, effect_text, has_ability_word_prefix) =
        if let Some((aw_name, effect_text)) = strip_ability_word_with_name(&line) {
            (ability_word_to_condition(&aw_name), effect_text, true)
        } else {
            (None, line.clone(), false)
        };

    Some(SpellResolutionLine {
        line,
        effect_text,
        ability_word_condition,
        has_ability_word_prefix,
    })
}

fn is_self_exile_cleanup_line(line: &str, card_name: &str) -> bool {
    let normalized = normalize_card_name_refs(line, card_name);
    let normalized_lower = normalized.to_lowercase();

    nom_on_lower(&normalized, &normalized_lower, |i| {
        value(
            (),
            (
                tag::<_, _, VerboseError<&str>>("exile "),
                tag::<_, _, VerboseError<&str>>("~"),
                opt(tag::<_, _, VerboseError<&str>>(".")),
            ),
        )
        .parse(i)
    })
    .is_some()
}

fn starts_with_until_duration(line: &str) -> bool {
    let lower = line.to_lowercase();
    nom_on_lower(line, &lower, |i| {
        value(
            (),
            alt((
                tag("until your next turn, "),
                tag("until the end of your next turn, "),
                tag("until end of turn, "),
            )),
        )
        .parse(i)
    })
    .is_some()
}

fn is_standalone_spell_keyword_action_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    let parsed = all_consuming(value(
        (),
        (
            tag::<_, _, VerboseError<&str>>("time travel"),
            opt(tag::<_, _, VerboseError<&str>>(".")),
        ),
    ))
    .parse(lower.as_str())
    .is_ok();
    parsed
}

fn is_semicolon_keyword_line(line: &str, mtgjson_keyword_names: &[String]) -> bool {
    let mut saw_multiple_parts = false;
    let mut parts = line
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty());
    let Some(first) = parts.next() else {
        return false;
    };

    if extract_keyword_line(first, mtgjson_keyword_names).is_none() {
        return false;
    }

    for part in parts {
        saw_multiple_parts = true;
        if extract_keyword_line(part, mtgjson_keyword_names).is_none() {
            return false;
        }
    }

    saw_multiple_parts
}

fn is_spell_resolution_instruction_line(
    prepared: &SpellResolutionLine,
    card_name: &str,
    mtgjson_keyword_names: &[String],
    parsed_so_far: &ParsedAbilities,
) -> bool {
    let line = &prepared.line;
    let lower = line.to_lowercase();

    if is_semicolon_keyword_line(line, mtgjson_keyword_names) {
        return false;
    }

    if lower == "start your engines!" || lower == "start your engines" {
        return false;
    }

    if lower == "your speed can increase beyond 4." || lower == "your speed can increase beyond 4" {
        return false;
    }

    if lower_starts_with(&lower, "equip")
        && !lower_starts_with(&lower, "equipped")
        && try_parse_equip(line).is_some()
    {
        return false;
    }

    if !is_ability_activate_cost_static(&lower)
        && extract_keyword_line(line, mtgjson_keyword_names).is_some()
    {
        return false;
    }

    if lower_starts_with(&lower, "enchant ") && !lower_starts_with(&lower, "enchanted ") {
        return false;
    }

    if is_commander_permission_sentence(line) || try_parse_loyalty_line(line).is_some() {
        return false;
    }

    if is_granted_static_line(&lower) {
        return false;
    }

    if nom_on_lower(line, &lower, |i| {
        value((), alt((tag("to solve \u{2014} "), tag("to solve -- ")))).parse(i)
    })
    .is_some()
    {
        return false;
    }

    if nom_on_lower(line, &lower, |i| {
        value((), alt((tag("solved \u{2014} "), tag("solved -- ")))).parse(i)
    })
    .is_some()
    {
        return false;
    }

    if nom_on_lower(line, &lower, |i| {
        value((), alt((tag("channel \u{2014} "), tag("channel -- ")))).parse(i)
    })
    .is_some()
    {
        return false;
    }

    if find_activated_colon(line).is_some() {
        return false;
    }

    let effect_lower = prepared.effect_text.to_lowercase();
    if has_trigger_prefix(&effect_lower) {
        return false;
    }

    if is_static_pattern(&effect_lower) && !should_defer_spell_to_effect(&effect_lower) {
        return false;
    }

    if is_replacement_pattern(&effect_lower)
        && !(scan_contains(&effect_lower, "prevent") && scan_contains(&effect_lower, "damage"))
        && parse_replacement_line(line, card_name).is_some()
    {
        return false;
    }

    if is_opening_hand_begin_game(&lower) || lower_starts_with(&lower, "as an additional cost") {
        return false;
    }

    if parsed_so_far.strive_cost.is_some() {
        if let Some(effect_text) = strip_ability_word(line) {
            let effect_lower = effect_text.to_lowercase();
            if lower_starts_with(&effect_lower, "this spell costs ")
                && scan_contains(
                    &effect_lower,
                    "more to cast for each target beyond the first",
                )
            {
                return false;
            }
        }
    }

    if parse_casting_restriction_line(line).is_some()
        || parse_spell_casting_option_line(line, card_name).is_some()
    {
        return false;
    }

    if is_saga_chapter(&lower)
        || is_flashback_equal_mana_cost(&lower)
        || lower_starts_with(&lower, "commander ninjutsu ")
        || lower_starts_with(&lower, "escape")
        || lower_starts_with(&lower, "cumulative upkeep")
        || is_keyword_cost_line(&lower)
        || is_vehicle_tier_line(&lower)
        || lower_starts_with(&lower, "activate ")
        || lower_starts_with(&lower, "suspend ")
        || lower_starts_with(&lower, "harmonize ")
        || lower_starts_with(&lower, "flashback")
        || lower_starts_with(&lower, "buyback")
        || lower_starts_with(&lower, "this spell costs ")
        || alt((
            tag::<_, _, VerboseError<&str>>("kicker"),
            tag("multikicker"),
            tag("replicate"),
            tag("mayhem"),
        ))
        .parse(lower.as_str())
        .is_ok()
    {
        return false;
    }

    let saved_warnings = take_warnings();
    let parsed = parse_effect_chain_with_context(
        &prepared.effect_text,
        AbilityKind::Spell,
        &ParseContext {
            subject: None,
            card_name: Some(card_name.to_string()),
            actor: None,
            ..Default::default()
        },
    );
    let _candidate_warnings = take_warnings();
    for warning in saved_warnings {
        push_warning(warning);
    }
    !has_unimplemented(&parsed)
}

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
/// - Ferocious: you control a creature with power 4 or greater
fn ability_word_to_condition(word: &str) -> Option<crate::types::ability::StaticCondition> {
    use crate::types::ability::{
        CardTypeSetSource, ControllerRef, CountScope, FilterProp, QuantityExpr, QuantityRef,
        StaticCondition, TargetFilter, TypeFilter, TypedFilter, ZoneRef,
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
                qty: QuantityRef::DistinctCardTypes {
                    source: CardTypeSetSource::Zone {
                        zone: ZoneRef::Graveyard,
                        scope: CountScope::Controller,
                    },
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
                    qty: QuantityRef::ZoneChangeCountThisTurn {
                        from: Some(Zone::Battlefield),
                        to: None,
                        filter: TargetFilter::Typed(
                            TypedFilter::permanent().controller(ControllerRef::You),
                        ),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
        }
        // allow-noncombinator: semantic mapping after ability-word parser has classified the word
        "ferocious" => Some(StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::PowerGE {
                                value: QuantityExpr::Fixed { value: 4 },
                            }]),
                    ),
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        }),
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

/// Single-authority merge for composing a freshly-parsed `AbilityCondition` onto an
/// existing one on an `AbilityDefinition`.
///
/// CR 608.2c: Compound condition — a spell's resolution gate is the conjunction of
/// every condition that applies. Two independent parser paths can emit the same
/// condition (e.g. the "Delirium —" ability-word prefix and the literal
/// "If there are four or more card types..." phrase both yield the same
/// `QuantityCheck`). Structural dedup keeps the AST flat and prevents
/// `And(X, X)` wrappers that would be semantically identical but waste work.
///
/// Invariants:
/// - Structural equality (`==`) is the dedup criterion.
/// - Results never nest: `And` children are always leaves, never `And`.
/// - Empty-conjunction not produced — at least one operand is always retained.
fn merge_ability_condition(
    existing: Option<crate::types::ability::AbilityCondition>,
    incoming: crate::types::ability::AbilityCondition,
) -> crate::types::ability::AbilityCondition {
    use crate::types::ability::AbilityCondition;
    match existing {
        None => incoming,
        Some(existing) if existing == incoming => existing,
        Some(AbilityCondition::And { mut conditions }) => {
            // Flatten: if incoming is itself an And, absorb its children.
            let new_children: Vec<AbilityCondition> = match incoming {
                AbilityCondition::And { conditions: inner } => inner,
                other => vec![other],
            };
            for child in new_children {
                if !conditions.contains(&child) {
                    conditions.push(child);
                }
            }
            // If dedup collapsed everything to a single child, unwrap.
            if conditions.len() == 1 {
                conditions.into_iter().next().unwrap()
            } else {
                AbilityCondition::And { conditions }
            }
        }
        Some(existing) => match incoming {
            AbilityCondition::And { mut conditions } => {
                // Existing is a leaf; prepend it to the incoming And (deduped).
                if !conditions.contains(&existing) {
                    conditions.insert(0, existing);
                }
                if conditions.len() == 1 {
                    conditions.into_iter().next().unwrap()
                } else {
                    AbilityCondition::And { conditions }
                }
            }
            other => AbilityCondition::And {
                conditions: vec![existing, other],
            },
        },
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

/// Parse Oracle text into structured ability definitions.
///
/// Splits on newlines, strips reminder text, then classifies each line
/// according to a priority table (keywords, enchant, equip, activated,
/// triggered, static, replacement, spell effect, modal, loyalty, etc.).
///
/// `mtgjson_keyword_names` are the raw lowercased keyword names from MTGJSON
/// (e.g. `["flying", "protection"]`). Used to identify keyword-only lines
/// and to avoid re-extracting keywords MTGJSON already provides.
///
/// # Self-reference normalization
///
/// This function is the **single normalization entry point** for the parser.
/// It invokes [`normalize_card_name_refs`] once on the raw Oracle text (CR
/// 201.4b) so every downstream block parser — saga chapters, class levels,
/// leveler blocks, modal mode bodies, triggers, statics, effects, replacements,
/// spacecraft thresholds — receives text with the card's self-references
/// rewritten to `~`.
///
/// The `pub fn` wrappers exposed for direct testing
/// (`parse_replacement_line`, `parse_trigger_line`, `parse_trigger_lines`,
/// `parse_class_oracle_text`, etc.) re-invoke `normalize_card_name_refs`
/// internally; when called via this function the re-invocation is an
/// idempotent no-op.
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
    clear_warnings();
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
        parse_warnings: Vec::new(),
    };

    // CR 201.4b: A card's Oracle text uses its name to refer to itself.
    // Normalize self-references to `~` once, at the single parser entry point,
    // so every downstream block parser (saga, class, leveler, modal, trigger,
    // static, effect, replacement, spacecraft) receives already-normalized
    // text. The `pub fn` wrappers retained for test-facing API re-invoke
    // `normalize_card_name_refs` on this pre-normalized text; strategies 1-4
    // find nothing to replace and strategy 5 is short-circuited by its
    // `!result.contains('~')` guard, making re-entry an idempotent no-op.
    let oracle_text_owned = normalize_card_name_refs(oracle_text, card_name);
    let lines: Vec<&str> = oracle_text_owned.split('\n').collect();

    // CR 714: Pre-parse Saga chapter lines into triggers + ETB replacement.
    let saga_consumed = if subtypes.iter().any(|s| s == "Saga") {
        let (chapter_triggers, etb_replacement, consumed) = parse_saga_chapters(&lines, card_name);
        result.triggers.extend(chapter_triggers);
        result.replacements.push(etb_replacement);
        consumed
    } else {
        std::collections::HashSet::new()
    };

    // CR 716: Pre-parse Class level sections into level-gated abilities.
    if subtypes.iter().any(|s| s == "Class") {
        let mut class_result =
            parse_class_oracle_text(&lines, card_name, mtgjson_keyword_names, result);
        class_result.parse_warnings = take_warnings();
        return class_result;
    }

    // CR 711: Pre-parse leveler LEVEL blocks into counter-gated static abilities.
    let (level_statics, level_consumed, level_ability_lines) =
        parse_level_blocks(&lines, card_name);
    if !level_statics.is_empty() {
        result.statics.extend(level_statics);
    }
    // CR 711.2a + CR 711.2b: Re-parse ability lines found within LEVEL blocks through
    // the normal trigger/activated/static pipeline, then attach the level counter condition.
    for (ability_text, level_condition) in &level_ability_lines {
        let (minimum, maximum) = match level_condition {
            StaticCondition::HasCounters {
                minimum, maximum, ..
            } => (*minimum, *maximum),
            _ => continue,
        };

        // CR 711.2a + CR 711.2b: Activated abilities within LEVEL blocks get a LevelCounterRange restriction.
        if let Some(colon_pos) = find_activated_colon(ability_text) {
            let cost_text = ability_text[..colon_pos].trim();
            let effect_text = ability_text[colon_pos + 1..].trim();
            let (effect_text, constraints) = strip_activated_constraints(effect_text);
            let normalized_cost_text = normalize_self_refs_for_static(cost_text, card_name);
            let cost = parse_oracle_cost(&normalized_cost_text);

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
            def.description = Some(ability_text.to_string());
            if constraints.sorcery_speed() {
                def.sorcery_speed = true;
            }
            let mut restrictions = constraints.restrictions;
            restrictions.push(ActivationRestriction::LevelCounterRange { minimum, maximum });
            def.activation_restrictions = restrictions;
            extract_cost_reduction_from_chain(&mut def);
            result.abilities.push(def);
            continue;
        }

        // CR 711.2a + CR 711.2b: Triggered abilities within LEVEL blocks get a HasCounters condition.
        // (Static abilities are now parsed directly in oracle_level.rs with the level condition attached.)
        let trigger_condition = TriggerCondition::HasCounters {
            counters: crate::types::counter::CounterMatch::OfType(
                crate::types::counter::CounterType::Generic("level".to_string()),
            ),
            minimum,
            maximum,
        };
        // CR 707.9a: Thread the running trigger count as the base index so
        // any "and it has this ability" except clause inside a leveler trigger
        // body resolves to the correct printed-trigger slot.
        let mut triggers =
            parse_trigger_lines_at_index(ability_text, card_name, Some(result.triggers.len()));
        for trigger in &mut triggers {
            trigger.condition = Some(trigger_condition.clone());
        }
        result.triggers.extend(triggers);
    }

    // CR 702.184a + CR 721.2: Pre-parse Spacecraft "N+ | body" threshold lines
    // into charge-counter-gated statics / triggers / activated abilities. The
    // `Station` reminder-text paragraph is handled independently: the keyword
    // itself comes from MTGJSON, and the creature-shift at the highest symbol
    // (CR 721.2b) is synthesized post-parse in `database::synthesis::synthesize_station`
    // where `face.power` / `face.toughness` are available for the base P/T.
    let spacecraft_consumed = if subtypes.iter().any(|s| s == "Spacecraft") {
        // CR 707.9a: Pass the running trigger count so any "has this ability"
        // retain modification inside a Spacecraft threshold trigger body
        // resolves to the correct printed-trigger slot.
        let (sc_statics, sc_triggers, sc_abilities, consumed) =
            parse_spacecraft_threshold_lines(&lines, card_name, result.triggers.len());
        result.statics.extend(sc_statics);
        result.triggers.extend(sc_triggers);
        for mut def in sc_abilities {
            extract_cost_reduction_from_chain(&mut def);
            result.abilities.push(def);
        }
        consumed
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
    } else {
        std::collections::HashSet::new()
    };

    // CR 207.2c + CR 601.2f: Pre-parse Strive ability word cost before main loop.
    // Strive lines have the form: "Strive — This spell costs {X} more to cast for each
    // target beyond the first." — extract the per-target surcharge cost.
    for raw in &lines {
        let stripped = strip_reminder_text(raw.trim());
        if let Some(effect_text) = strip_ability_word(&stripped) {
            let effect_lower = effect_text.to_lowercase();
            if let Some(((), rest_original)) = nom_on_lower(&effect_text, &effect_lower, |i| {
                value((), tag("this spell costs ")).parse(i)
            }) {
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
        // CR 711: Skip lines already consumed by the leveler pre-parser.
        if level_consumed.contains(&i) {
            i += 1;
            continue;
        }
        // CR 714: Skip lines already consumed by the saga pre-parser.
        if saga_consumed.contains(&i) {
            i += 1;
            continue;
        }
        // CR 702.184a + CR 721: Skip Spacecraft threshold lines already consumed.
        if spacecraft_consumed.contains(&i) {
            i += 1;
            continue;
        }

        let raw_line = lines[i].trim();
        if raw_line.is_empty() {
            i += 1;
            continue;
        }

        // CR 207.2c: Ability words have no rules meaning. For the Increment-class
        // pattern (`<ability-word> (<body>)`) where the printed reminder text IS
        // the rules body — e.g., SOS Increment / Opus / Repartee / Converge —
        // extract the parenthesized body and dispatch it as if it were the line
        // itself. Without this, `strip_reminder_text` (next line) would erase
        // the entire body and leave only the bare ability-word name, producing
        // zero parsed abilities for these cards.
        let reminder_body_owned = extract_ability_word_reminder_body(raw_line);
        let raw_line: &str = reminder_body_owned.as_deref().unwrap_or(raw_line);

        let line = strip_reminder_text(raw_line);
        // Strip "X can't be 0." casting constraint suffix — annotation only, not an ability.
        let line = strip_x_cant_be_zero_suffix(&line);
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

        // CR 702.xxx: Prepare (Strixhaven) — `~ enters prepared.` is a self-ETB
        // rider shorthand (analogous to `enters tapped` / `enters transformed`)
        // that synthesizes a self-ETB trigger whose effect is BecomePrepared.
        // Delegated to the oracle_trigger combinator; nom-composed detection.
        // Normalize self-refs first so lines like "Lluwen enters prepared." (where
        // the short card name is still the subject) reach the `~`-gated combinator.
        let prepared_normalized = normalize_self_refs_for_static(&line, card_name);
        if let Some(mut trigger) =
            super::oracle_trigger::try_parse_enters_prepared_rider(&prepared_normalized)
        {
            trigger.description = Some(line.clone());
            result.triggers.push(trigger);
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

        let lower = line.to_lowercase();

        // Pre-keyword activated ability: "Equip {cost}" / "Equip — {cost}"
        // (but not "Equipped ...").
        // This must run before keyword-only extraction because MTGJSON keyword
        // names can match exact printed equip costs, but equip is an activated
        // ability and still needs the synthesized activation body.
        if lower_starts_with(&lower, "equip") && !lower_starts_with(&lower, "equipped") {
            if let Some(ability) = try_parse_equip(&line) {
                result.abilities.push(ability);
                i += 1;
                continue;
            }
        }

        // Priority 1b: keyword-only line — extract any keywords for the union set
        // Guard: "{Keyword} abilities you activate cost {N} less" is a static ability,
        // not a keyword line. Don't let keyword extraction consume it.
        let is_ability_cost_static = is_ability_activate_cost_static(&lower);
        if !is_ability_cost_static {
            if let Some(extracted) = extract_keyword_line(&line, mtgjson_keyword_names) {
                result.extracted_keywords.extend(extracted);
                i += 1;
                continue;
            }
        }

        // Normalize card self-references for static parsing (replace card name with ~)
        let static_line = normalize_self_refs_for_static(&line, card_name);
        if let Some(next_raw_line) = lines.get(i + 1).map(|next| next.trim()) {
            if !next_raw_line.is_empty() {
                let next_line = strip_x_cant_be_zero_suffix(&strip_reminder_text(next_raw_line));
                if !next_line.is_empty() {
                    let next_static_line = normalize_self_refs_for_static(&next_line, card_name);
                    let combined_static_line = format!("{static_line} {next_static_line}");
                    if let Some(static_def) =
                        try_parse_graveyard_keyword_static_with_continuation(&combined_static_line)
                    {
                        result.statics.push(static_def);
                        i += 2;
                        continue;
                    }
                }
            }
        }

        if lower == "start your engines!" || lower == "start your engines" {
            result.extracted_keywords.push(Keyword::StartYourEngines);
            i += 1;
            continue;
        }

        if lower == "your speed can increase beyond 4."
            || lower == "your speed can increase beyond 4"
        {
            let defs = parse_static_line_with_graveyard_keyword_continuation(&static_line);
            if !defs.is_empty() {
                result.statics.extend(defs);
                i += 1;
                continue;
            }
        }

        // Priority 2: "Enchant {filter}" — skip (handled externally)
        if lower_starts_with(&lower, "enchant ") && !lower_starts_with(&lower, "enchanted ") {
            i += 1;
            continue;
        }

        if is_commander_permission_sentence(&line) {
            i += 1;
            continue;
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
            if is_cant_win_lose_compound(&lower) {
                for clause in static_line.split(" and ") {
                    let trimmed = clause.trim().trim_end_matches('.');
                    if !trimmed.is_empty() {
                        let clause_dot = format!("{trimmed}.");
                        result.statics.extend(
                            parse_static_line_with_graveyard_keyword_continuation(&clause_dot),
                        );
                    }
                }
                i += 1;
                continue;
            }
            // Compound detection (CR 602.5 can't-be-activated, cross-mode conjunctions,
            // life-total locks, etc.) is already owned by `parse_static_line_multi`,
            // which the wrapper below delegates to.
            let defs = parse_static_line_with_graveyard_keyword_continuation(&static_line);
            if !defs.is_empty() {
                result.statics.extend(defs);
                i += 1;
                continue;
            }
        }

        // Priority 3b: Case "To solve — {condition}" line (CR 719.1)
        if let Some(((), rest_original)) = nom_on_lower(&line, &lower, |i| {
            value((), alt((tag("to solve \u{2014} "), tag("to solve -- ")))).parse(i)
        }) {
            let rest_lower = rest_original.to_lowercase();
            result.solve_condition = Some(parse_solve_condition(&rest_lower));
            i += 1;
            continue;
        }

        // CR 719.3c: Case "Solved — {cost}: {effect}" activated ability.
        if let Some(((), rest)) = nom_on_lower(&line, &lower, |i| {
            value((), alt((tag("solved \u{2014} "), tag("solved -- ")))).parse(i)
        }) {
            if let Some(colon_pos) = find_activated_colon(rest) {
                let cost_text = rest[..colon_pos].trim();
                let effect_text = rest[colon_pos + 1..].trim();
                let (effect_text, constraints) = strip_activated_constraints(effect_text);
                let cost = parse_oracle_cost(cost_text);

                let mut def = parse_effect_chain(&effect_text, AbilityKind::Activated);
                def.cost = Some(cost);
                def.description = Some(line.to_string());
                // CR 719.3c: Solved abilities only activate while Case is solved.
                def.activation_restrictions
                    .push(ActivationRestriction::IsSolved);
                if constraints.sorcery_speed() {
                    def.sorcery_speed = true;
                }
                // CR 602.5d: `constraints.restrictions` already contains
                // `AsSorcery` when the source text said "Activate only as a
                // sorcery"; extend preserves it so the legality gate fires.
                if !constraints.restrictions.is_empty() {
                    def.activation_restrictions.extend(constraints.restrictions);
                }
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Priority 3c: Channel — "Channel — {cost}, Discard this card: {effect}" (CR 207.2c + CR 602.1)
        if let Some(((), rest_original)) = nom_on_lower(&line, &lower, |i| {
            value((), alt((tag("channel \u{2014} "), tag("channel -- ")))).parse(i)
        }) {
            let rest_lower = rest_original.to_lowercase();
            if let Some(colon_pos) = find_activated_colon(&rest_lower) {
                let prefix_len = line.len() - rest_original.len();
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

            // Retry with `~` normalization if the first pass left an
            // Unimplemented node or emitted a `target-fallback` warning
            // (Metalhead class: PutCounter silently fell back to `Any`).
            let mut def = parse_activated_with_self_ref_fallback(&effect_text, card_name);
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
            if has_roll_die_pattern(&effect_text.to_lowercase()) {
                i = attach_die_result_branches_to_chain(&mut def, &lines, i);
            }
            result.abilities.push(def);
            continue;
        }

        // Priority 5-pre: "Whenever you cast [spell], that [subject] enters with
        // [counters] on it" is a replacement effect per CR 614.1c, not a
        // triggered ability — despite the "whenever" framing. Intercept before
        // the generic trigger dispatch routes it through the SpellCast matcher.
        // Applies to Wildgrowth Archaic and cousin cards (Runadi, Boreal
        // Outrider, Torgal, …). `parse_replacement_line` handles all the
        // compositional variants (fixed / X / "where X is …").
        if has_trigger_prefix(&lower) && scan_contains(&lower, "enters with") {
            if let Some(rep_def) = parse_replacement_line(&line, card_name) {
                result.replacements.push(rep_def);
                i += 1;
                continue;
            }
        }

        // Priority 5-6: Triggered abilities — starts with When/Whenever/At
        // CR 603.2: Compound triggers ("When X and when Y, effect") produce
        // multiple TriggerDefinitions sharing the same execute effect.
        if has_trigger_prefix(&lower) {
            // CR 707.9a: Pass the running trigger count as the base index so
            // any "and it has this ability" except clause in this trigger's
            // body resolves to the correct printed-trigger slot.
            let mut triggers =
                parse_trigger_lines_at_index(&line, card_name, Some(result.triggers.len()));
            i += 1;
            // CR 706: If the trigger's effect ends with "roll a dN", consume
            // subsequent d20 table lines and attach them as die result branches.
            if has_roll_die_pattern(&lower) {
                if let Some(last) = triggers.last_mut() {
                    if let Some(ref mut execute) = last.execute {
                        i = attach_die_result_branches_to_chain(execute, &lines, i);
                    }
                }
            }
            result.triggers.extend(triggers);
            continue;
        }

        // Priority 6b: Ability-word-prefixed triggers (e.g., "Heroic — Whenever ...",
        // "Constellation — Whenever ..."). Must intercept BEFORE is_static_pattern and
        // is_replacement_pattern checks, which would otherwise match on keywords like
        // "prevent" in the effect text and misroute the line.
        if let Some((aw_name, effect_text)) = strip_ability_word_with_name(&line) {
            let effect_lower = effect_text.to_lowercase();
            if has_trigger_prefix(&effect_lower) {
                // CR 707.9a: Thread the running trigger count as the base index.
                let mut triggers = parse_trigger_lines_at_index(
                    &effect_text,
                    card_name,
                    Some(result.triggers.len()),
                );
                // B7: Attach ability-word condition as fallback when extract_if_condition
                // doesn't recognize the intervening-if pattern.
                for trigger in &mut triggers {
                    if trigger.condition.is_none() {
                        trigger.condition = ability_word_to_trigger_condition(&aw_name);
                    }
                }
                i += 1;
                if has_roll_die_pattern(&effect_lower) {
                    if let Some(last) = triggers.last_mut() {
                        if let Some(ref mut execute) = last.execute {
                            i = attach_die_result_branches_to_chain(execute, &lines, i);
                        }
                    }
                }
                result.triggers.extend(triggers);
                continue;
            }
        }

        // CR 701.43d: "You may exert [creature] as it attacks" — optional attack cost.
        // Must intercept BEFORE Priority 7 (static patterns) because the "When you do"
        // linked effect often contains "gets +N/+M" which is_static_pattern would match.
        // Standalone: skip (separate "Whenever you exert" trigger line follows).
        // Compound: produce an Exerted trigger with the linked effect.
        if let Some(((), rest_original)) = nom_on_lower(&line, &lower, |i| {
            value(
                (),
                alt((
                    tag("you may exert this creature as it attacks"),
                    tag("you may exert ~ as it attacks"),
                    tag("you may exert it as it attacks"),
                )),
            )
            .parse(i)
        }) {
            // Check for linked "When you do, [effect]" in same sentence
            let rest_trimmed = rest_original.trim().trim_start_matches('.').trim_start();
            let rest_lower = rest_trimmed.to_lowercase();
            if let Some(((), effect_rest)) = nom_on_lower(rest_trimmed, &rest_lower, |i| {
                value((), tag("when you do, ")).parse(i)
            }) {
                let effect_def = parse_effect_chain(effect_rest.trim(), AbilityKind::Spell);
                let trigger = TriggerDefinition::new(TriggerMode::Exerted)
                    .valid_card(TargetFilter::SelfRef)
                    .trigger_zones(vec![Zone::Battlefield])
                    .execute(effect_def)
                    .description(line.to_string());
                result.triggers.push(trigger);
            }
            i += 1;
            continue;
        }
        // CR 701.43d: Variant with card name — "You may exert {Name} as {he/she/it/they} attacks"
        if nom_on_lower(&line, &lower, |i| value((), tag("you may exert ")).parse(i)).is_some()
            && scan_contains(&lower, "as ")
            && scan_contains(&lower, "attacks")
        {
            if let Some((_, effect_text)) = split_once_on_lower(&line, &lower, ". when you do, ") {
                let effect_def = parse_effect_chain(effect_text.trim(), AbilityKind::Spell);
                let trigger = TriggerDefinition::new(TriggerMode::Exerted)
                    .valid_card(TargetFilter::SelfRef)
                    .trigger_zones(vec![Zone::Battlefield])
                    .execute(effect_def)
                    .description(line.to_string());
                result.triggers.push(trigger);
            }
            i += 1;
            continue;
        }
        // CR 701.43d: Conditional exert — "If [creature] hasn't been exerted this turn, you may exert it"
        if nom_on_lower(&line, &lower, |i| value((), tag("if ")).parse(i)).is_some()
            && scan_contains(&lower, "you may exert")
            && scan_contains(&lower, "attacks")
        {
            if let Some((_, effect_text)) = split_once_on_lower(&line, &lower, ". when you do, ") {
                let effect_def = parse_effect_chain(effect_text.trim(), AbilityKind::Spell);
                let trigger = TriggerDefinition::new(TriggerMode::Exerted)
                    .valid_card(TargetFilter::SelfRef)
                    .trigger_zones(vec![Zone::Battlefield])
                    .execute(effect_def)
                    .description(line.to_string());
                result.triggers.push(trigger);
            }
            i += 1;
            continue;
        }

        // Priority 6c-defiler: "As an additional cost to cast [color] permanent spells,
        // you may pay N life. Those spells cost {C} less to cast if you paid life this way."
        // This is a static ability on the permanent, not a self-cost for this spell.
        if is_defiler_cost_pattern(&lower) {
            if let Some((static_def, consumes_next_line)) =
                parse_defiler_cost_reduction(&lower, i + 1 < lines.len(), || {
                    lines.get(i + 1).map(|l| l.to_lowercase())
                })
            {
                result.statics.push(static_def);
                i += if consumes_next_line { 2 } else { 1 };
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
            if lower_starts_with(&lower, "as long as ") && is_replacement_pattern(&lower) {
                if let Some(rep_def) = parse_replacement_line(&line, card_name) {
                    result.replacements.push(rep_def);
                    i += 1;
                    continue;
                }
            }
            // Guard: ability-word-prefixed trigger lines (e.g., "Flurry — Whenever...")
            // handled above at Priority 6b. The check below is kept as a defensive
            // guard for any edge cases that reach Priority 7.
            let is_ability_word_trigger = strip_ability_word(&line).is_some_and(|stripped| {
                let sl = stripped.to_lowercase();
                has_trigger_prefix(&sl)
            });
            let defer_to_effect_parser =
                is_ability_word_trigger || (is_spell && should_defer_spell_to_effect(&lower));
            if !defer_to_effect_parser {
                // B7: Ability-word-prefixed static lines — strip prefix and attach condition.
                // Must happen here (Priority 7) because Priority 9 (spell catch-all) would
                // otherwise consume the line before Priority 14 for instants/sorceries.
                if let Some((aw_name, effect_text)) = strip_ability_word_with_name(&line) {
                    let effect_static = normalize_self_refs_for_static(&effect_text, card_name);
                    let mut defs =
                        parse_static_line_with_graveyard_keyword_continuation(&effect_static);
                    if !defs.is_empty() {
                        if let Some(cond) = ability_word_to_condition(&aw_name) {
                            for def in &mut defs {
                                if def.condition.is_none() {
                                    def.condition = Some(cond.clone());
                                }
                            }
                        }
                        for def in &mut defs {
                            def.description = Some(line.to_string());
                        }
                        result.statics.extend(defs);
                        i += 1;
                        continue;
                    }
                }
                // B20: Handle compound "can't win/lose" lines by splitting
                // at " and " so both CantWinTheGame and CantLoseTheGame emit.
                // CR 104.3a / CR 104.3b: Both restrictions must be independent statics.
                if is_cant_win_lose_compound(&lower) {
                    for clause in static_line.split(" and ") {
                        let trimmed = clause.trim().trim_end_matches('.');
                        if !trimmed.is_empty() {
                            let clause_dot = format!("{trimmed}.");
                            result.statics.extend(
                                parse_static_line_with_graveyard_keyword_continuation(&clause_dot),
                            );
                        }
                    }
                    i += 1;
                    continue;
                }
                // Compound clause: casting time restriction + per-turn limit joined by " and "
                // E.g., Fires of Invention: "You can cast spells only during your turn and
                // you can cast no more than two spells each turn."
                // CR 117.1a + CR 604.1: Both restrictions are independent statics.
                if is_compound_turn_limit(&lower) {
                    for clause in static_line.split(" and ") {
                        let trimmed = clause.trim().trim_end_matches('.');
                        if !trimmed.is_empty() {
                            let clause_dot = format!("{trimmed}.");
                            result.statics.extend(
                                parse_static_line_with_graveyard_keyword_continuation(&clause_dot),
                            );
                        }
                    }
                    i += 1;
                    continue;
                }
                // Compound detection (CR 602.5 can't-be-activated, cross-mode conjunctions,
                // "attacks or blocks each combat if able" → MustAttack + MustBlock, life-total
                // locks, etc.) is already owned by `parse_static_line_multi`, which the wrapper
                // delegates to.
                let defs = parse_static_line_with_graveyard_keyword_continuation(&static_line);
                if !defs.is_empty() {
                    result.statics.extend(defs);
                    i += 1;
                    continue;
                }
            }
        }

        // CR 615 + CR 105.1: "Prevent all damage that sources of the color of your choice
        // would deal this turn." → Choose(Color) → PreventDamage chain.
        // Must run before Priority 8 (replacement) to avoid being caught as a passive shield.
        if is_spell
            && scan_contains(&lower, "prevent")
            && scan_contains(&lower, "damage")
            && scan_contains(&lower, "color of your choice")
        {
            use crate::types::ability::{
                ChoiceType, FilterProp, PreventionAmount, PreventionScope,
            };
            // CR 615 + CR 105.1: Build a source filter using IsChosenColor —
            // at resolution time the resolver reads ChosenAttribute::Color from
            // the source object and converts to a concrete HasColor filter.
            let mut source_filter = TypedFilter::default();
            source_filter.properties.push(FilterProp::IsChosenColor);
            let def = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Choose {
                    choice_type: ChoiceType::Color,
                    persist: true,
                },
            )
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PreventDamage {
                    amount: PreventionAmount::All,
                    target: TargetFilter::Any,
                    scope: PreventionScope::AllDamage,
                    damage_source_filter: Some(TargetFilter::Typed(source_filter)),
                },
            ))
            .description(line.to_string());
            result.abilities.push(def);
            i += 1;
            continue;
        }

        // Instant/sorcery prevention text creates a resolving spell effect,
        // not a standing replacement definition. Let the effect-chain parser
        // preserve any preceding clauses ("You gain 1 life for each ...")
        // before the replacement classifier sees the prevention marker.
        if is_spell && scan_contains(&lower, "prevent") && scan_contains(&lower, "damage") {
            let def = parse_effect_chain_with_context(
                &line,
                AbilityKind::Spell,
                &ParseContext {
                    subject: None,
                    card_name: Some(card_name.to_string()),
                    actor: None,
                    ..Default::default()
                },
            );
            if !has_unimplemented(&def) {
                result.abilities.push(def);
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
        // CR 103.6: The Leyline rule — opt-in at game start, never compelled.
        if is_opening_hand_begin_game(&lower) {
            let mut def = AbilityDefinition::new(
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
                    up_to: false,
                    enter_with_counters: vec![],
                },
            )
            .description(line.to_string());
            def.optional = true;
            result.abilities.push(def);
            i += 1;
            continue;
        }

        // Priority 8b: "As an additional cost to cast this spell"
        if lower_starts_with(&lower, "as an additional cost") {
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
                if lower_starts_with(&effect_lower, "this spell costs ")
                    && effect_lower.contains("more to cast for each target beyond the first")
                {
                    i += 1;
                    continue;
                }
            }
        }

        // CR 601.3: "Cast this spell only [condition]" — applies to any card type, not just instants/sorceries.
        if let Some(restrictions) = parse_casting_restriction_line(&line) {
            result.casting_restrictions.extend(restrictions);
            i += 1;
            continue;
        }

        if let Some(option) = parse_spell_casting_option_line(&line, card_name) {
            result.casting_options.push(option);
            i += 1;
            continue;
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

        // CR 702.62a: Suspend N—{cost} — parse count and cost from Oracle text.
        // Must run before the spell imperative catch-all (priority 9) so the line
        // is intercepted as a keyword, not parsed as an Unimplemented ability.
        // Spells (instants/sorceries) with Suspend would otherwise be caught by
        // the is_spell branch and produce an Unimplemented effect.
        if lower_starts_with(&lower, "suspend ") {
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // Harmonize {cost} — parse mana cost from Oracle text.
        // Must run before the spell imperative catch-all (priority 9) so the line
        // is intercepted as a keyword, not parsed as an effect.
        // MTGJSON keywords array only says "Harmonize" (no cost), so we extract cost here.
        // Format: "Harmonize {cost} (reminder text)" — space-separated.
        // Note: When MTGJSON provides "Harmonize" in keywords, extract_keyword_line at
        // priority 1b already handles this. This is a fallback for test/edge cases.
        if lower_starts_with(&lower, "harmonize ") {
            if let Some(harmonize_kw) = parse_harmonize_keyword(&line) {
                result.extracted_keywords.push(harmonize_kw);
                i += 1;
                continue;
            }
        }

        // Priority 8f: Kicker / Multikicker / Replicate cost lines — must run BEFORE Priority 9
        // (spell catch-all) so these keyword declarations on spell cards don't become Unimplemented.
        // We cannot use is_keyword_cost_line here because it would also catch "escape", "flashback",
        // etc. whose specific em-dash parsers run between Priority 9 and Priority 13.
        // Note: "mayhem" IS in is_keyword_cost_line and is handled at Priority 1b via MTGJSON
        // keywords when present; this guard catches it when keywords[] is empty.
        if alt((
            tag::<_, _, VerboseError<&str>>("kicker"),
            tag("multikicker"),
            tag("replicate"),
            tag("mayhem"),
        ))
        .parse(lower.as_str())
        .is_ok()
        {
            if let Some(cost) = parse_kicker_additional_cost_line(&line, &lower) {
                merge_kicker_additional_cost(&mut result.additional_cost, cost);
            }
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                result.extracted_keywords.push(kw);
            }
            i += 1;
            continue;
        }

        // CR 702.34a: Flashback em-dash form — "Flashback—{cost}", "Flashback—Tap N
        // creatures...", or compound "Flashback—{mana}, Pay N life." The comma in
        // compound costs prevents `extract_keyword_line` (priority 1b) from
        // recognising the line as a keyword-only line, and Priority 9 would
        // otherwise route it to the spell-effect catch-all and produce
        // `Unimplemented`. Intercept it here, before the spell catch-all, and
        // delegate to `parse_keyword_from_oracle`'s em-dash dispatcher.
        if lower_starts_with(&lower, "flashback") && line.contains('\u{2014}') {
            // Strip trailing punctuation so the em-dash dispatcher sees a clean
            // cost string. Reminder text was already removed by `strip_reminder_text`
            // upstream, but the trailing period from "Pay 3 life." remains.
            let lower_clean = lower.trim_end_matches('.').trim();
            if let Some(kw) = parse_keyword_from_oracle(lower_clean) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // CR 702.27a: Buyback em-dash form — "Buyback—Sacrifice a land." (Constant
        // Mists) etc. MTGJSON omits the Buyback keyword when the cost is non-mana,
        // so `extract_keyword_line` bails and the line would otherwise fall through
        // to the spell-effect catch-all and produce `Unimplemented`. Intercept here
        // before the spell catch-all, mirroring the Flashback em-dash intercept above.
        // structural: not dispatch — em-dash char presence gates the cost sub-parser,
        // which uses nom combinators in `parse_buyback_cost` / `parse_oracle_cost`.
        if lower_starts_with(&lower, "buyback") && line.contains('\u{2014}') {
            let lower_clean = lower.trim_end_matches('.').trim();
            if let Some(kw) = parse_keyword_from_oracle(lower_clean) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // Priority 9: Imperative verb for instants/sorceries
        if is_spell {
            // B7: Strip ability-word prefix and attach condition for spell effects.
            let mut spell_body_lines = Vec::new();
            let mut spell_description_lines = Vec::new();
            let Some(prepared_line) = prepare_spell_resolution_line(&line) else {
                i += 1;
                continue;
            };
            let aw_condition = prepared_line.ability_word_condition.clone();
            spell_body_lines.push(prepared_line.effect_text.clone());
            spell_description_lines.push(prepared_line.line);

            let mut next_i = i + 1;
            while next_i < lines.len() {
                if level_consumed.contains(&next_i)
                    || saga_consumed.contains(&next_i)
                    || spacecraft_consumed.contains(&next_i)
                    || parse_oracle_block(&lines, next_i).is_some()
                {
                    break;
                }

                let Some(next_prepared) = prepare_spell_resolution_line(lines[next_i]) else {
                    break;
                };

                if next_prepared.has_ability_word_prefix
                    || starts_with_until_duration(&next_prepared.effect_text)
                    || is_self_exile_cleanup_line(&next_prepared.effect_text, card_name)
                    || is_standalone_spell_keyword_action_line(&prepared_line.effect_text)
                    || !is_spell_resolution_instruction_line(
                        &next_prepared,
                        card_name,
                        mtgjson_keyword_names,
                        &result,
                    )
                {
                    break;
                }

                spell_body_lines.push(next_prepared.effect_text);
                spell_description_lines.push(next_prepared.line);
                next_i += 1;
            }

            let effect_line = spell_body_lines.join(" ");
            let description = spell_description_lines.join("\n");
            // CR 608.2c: Pre-strip "instead if [condition]" or trailing "instead"
            // from the effect text before chain parsing. This allows
            // strip_mana_value_conditional inside the chain parser to handle
            // mid-position MV conditions (e.g., "if it has mana value 4 or less")
            // that precede "instead if [ability word condition]".
            let (effect_line_clean, instead_condition, is_instead) =
                strip_instead_suffix(&effect_line);
            let parse_line = if is_instead {
                effect_line_clean.as_str()
            } else {
                effect_line.as_str()
            };
            let mut def = parse_effect_chain_with_context(
                parse_line,
                AbilityKind::Spell,
                &ParseContext {
                    subject: None,
                    card_name: Some(card_name.to_string()),
                    actor: None,
                    ..Default::default()
                },
            );
            def.description = Some(description);
            // CR 608.2c: Compose ability word condition with chain-extracted condition.
            // When both exist (e.g., Revolt + MV ≤ 4), compose through
            // `merge_ability_condition` which dedupes structurally-equal conditions
            // (e.g., "Delirium —" ability word + literal "if there are four or more
            // card types..." phrase both emit the same `QuantityCheck`) and flattens
            // nested `And` trees.
            // Ability-word condition (if any) is the "existing" baseline —
            // the chain-extracted condition is merged onto it, preserving the
            // historical `[ability_word, chain]` ordering when both are distinct.
            let chain = def.condition.take();
            def.condition = match (ability_word_to_ability_condition(&aw_condition), chain) {
                (Some(aw), Some(chain)) => Some(merge_ability_condition(Some(aw), chain)),
                (Some(aw), None) => Some(aw),
                (None, chain) => chain,
            };
            if let Some(instead_condition) = instead_condition {
                def.condition = Some(merge_ability_condition(
                    def.condition.take(),
                    instead_condition,
                ));
            }
            i = next_i;
            // CR 706: If the parsed chain ends with "roll a dN", consume
            // subsequent d20 table lines and attach them as die result branches.
            if has_roll_die_pattern(&lower) {
                i = attach_die_result_branches_to_chain(&mut def, &lines, i);
            }
            // CR 608.2c: Cross-line "instead" replacement — when a conditional line
            // replaces the entire preceding ability, compose them so the engine resolves
            // the binary choice correctly. The "instead" sub has the condition; the base
            // ability becomes the fallback when the condition is not met.
            if is_instead || is_instead_replacement_line(&effect_line) {
                if let Some(condition) = def.condition.take() {
                    if let Some(mut base) = result.abilities.pop() {
                        // Save the base ability's continuation chain in else_ability
                        // so the engine can run it when the condition is NOT met.
                        def.condition = Some(AbilityCondition::ConditionInstead {
                            inner: Box::new(condition),
                        });
                        def.else_ability = base.sub_ability.take();
                        base.sub_ability = Some(Box::new(def));
                        result.abilities.push(base);
                        continue;
                    }
                    // No previous ability to compose with — restore condition and push standalone.
                    def.condition = Some(condition);
                }
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
        if is_flashback_equal_mana_cost(&lower) {
            if parsed_result_recently_granted_flashback(&result) {
                i += 1;
                continue;
            }
            result.extracted_keywords.push(Keyword::Flashback(
                crate::types::keywords::FlashbackCost::Mana(
                    crate::types::mana::ManaCost::SelfManaCost,
                ),
            ));
            i += 1;
            continue;
        }

        // CR 702.49d: Commander ninjutsu is not in MTGJSON keywords — extract explicitly.
        if lower_starts_with(&lower, "commander ninjutsu ") {
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // CR 702.138: Escape — parse cost and exile count from Oracle text.
        // Must run before is_keyword_cost_line so the em-dash format is intercepted.
        if lower_starts_with(&lower, "escape") && line.contains('\u{2014}') {
            if let Some(escape_kw) = parse_escape_keyword(&line) {
                result.extracted_keywords.push(escape_kw);
                i += 1;
                continue;
            }
        }

        // CR 702.24: Cumulative upkeep — parse cost from Oracle text.
        // Must run before is_keyword_cost_line so the line is not silently skipped.
        // Format: "Cumulative upkeep—[cost]" or "Cumulative upkeep {mana}" (space-separated).
        if lower_starts_with(&lower, "cumulative upkeep") {
            if let Some(kw) = parse_cumulative_upkeep_keyword(&line) {
                result.extracted_keywords.push(kw);
                i += 1;
                continue;
            }
        }

        // Priority 13: Keyword cost lines — extract keyword if parseable, then skip.
        // MTGJSON provides keyword names (e.g. "Morph") but not parameterized forms.
        // The Oracle text has the full form (e.g. "Morph {2}{B}{G}{U}") which we extract here.
        if is_keyword_cost_line(&lower) {
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                result.extracted_keywords.push(kw);
            }
            i += 1;
            continue;
        }

        // Priority 13b: Kicker/Multikicker — skip (handled by keywords)
        if alt((
            tag::<_, _, VerboseError<&str>>("kicker"),
            tag("multikicker"),
        ))
        .parse(lower.as_str())
        .is_ok()
        {
            i += 1;
            continue;
        }

        // Priority 13c: Vehicle tier lines "N+ | keyword(s)" — skip (conditional stat grant)
        if is_vehicle_tier_line(&lower) {
            i += 1;
            continue;
        }

        // Priority 13d: "Activate only..." constraint — skip
        if lower_starts_with(&lower, "activate ") {
            i += 1;
            continue;
        }

        // Priority 13e: "X can't be 0." — casting constraint annotation, not an ability.
        // These appear as standalone lines on X-cost spells. The engine does not yet
        // enforce X-minimum restrictions, but recognizing this pattern prevents
        // Unimplemented fallback.
        if lower.trim_end_matches('.') == "x can't be 0" {
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
            if has_trigger_prefix(&effect_lower) {
                // CR 707.9a: Thread the running trigger count as the base index.
                let mut triggers = parse_trigger_lines_at_index(
                    &effect_text,
                    card_name,
                    Some(result.triggers.len()),
                );
                i += 1;
                // CR 706: Consume subsequent d20 table lines for triggered die rolls.
                if has_roll_die_pattern(&effect_lower) {
                    if let Some(last) = triggers.last_mut() {
                        if let Some(ref mut execute) = last.execute {
                            i = attach_die_result_branches_to_chain(execute, &lines, i);
                        }
                    }
                }
                result.triggers.extend(triggers);
                continue;
            }
            // Try as static
            if is_static_pattern(&effect_lower) {
                let effect_static = normalize_self_refs_for_static(&effect_text, card_name);
                let mut defs =
                    parse_static_line_with_graveyard_keyword_continuation(&effect_static);
                if !defs.is_empty() {
                    if let Some(cond) = aw_condition.clone() {
                        for def in &mut defs {
                            if def.condition.is_none() {
                                def.condition = Some(cond.clone());
                            }
                        }
                    }
                    result.statics.extend(defs);
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
                    actor: None,
                    ..Default::default()
                },
            );
            if !has_unimplemented(&def) {
                result.abilities.push(def);
                i += 1;
                continue;
            }
        }

        // Leftover permanent text can still be a valid static even when classifier
        // heuristics miss it. Try the actual static parser before falling through
        // to generic dispatch/unimplemented categorization.
        let static_line = normalize_self_refs_for_static(&line, card_name);
        let defs = parse_static_line_with_graveyard_keyword_continuation(&static_line);
        if !defs.is_empty() {
            result.statics.extend(defs);
            i += 1;
            continue;
        }

        // Priority 14a: Nom dispatch — try effect, trigger, static, and replacement
        // sub-parsers. If any succeeds, use the result directly.
        let nom_effect = dispatch_line_nom(&line, card_name);
        if !matches!(nom_effect, Effect::Unimplemented { .. }) {
            result
                .abilities
                .push(AbilityDefinition::new(AbilityKind::Spell, nom_effect));
            i += 1;
            continue;
        }

        // Priority 15: Final fallback — wrap as Unimplemented with diagnostic trace.
        result
            .abilities
            .push(make_unimplemented_with_effect(&line, nom_effect));
        i += 1;
    }

    // Architectural rule: the parser must never silently discard Oracle
    // text. Run the swallow audit against the parsed result so any unrep-
    // resented clause surfaces as a parse_warning instead of disappearing
    // (Phase 1: observability only — see swallow_check.rs for detector
    // catalog and Phase 2 demotion plan).
    super::swallow_check::check_swallowed_clauses(oracle_text, &result);
    result.parse_warnings = take_warnings();
    result
}

/// Try to parse "Equip {cost}" or "Equip — {cost}" lines.
/// Caller must verify the line starts with "equip" (case-insensitive) before calling.
///
/// CR 702.6a: Equip is the keyword. Distinct from "equipment" (a subtype noun)
/// and "equipped" (the static-grant subject) — both of which begin with the
/// same five letters. The caller's `lower_starts_with("equip")` check matches
/// all three; this function defends with a word-boundary guard so
/// "Equipment you control have equip {0}" (Puresteel Paladin granted-equip
/// pattern) does not slice off the first 5 bytes of "Equipment" and parse the
/// remainder ("ment you control...") as a malformed activated ability cost.
fn try_parse_equip(line: &str) -> Option<AbilityDefinition> {
    let (activation_line, cost_reduction) = split_trailing_self_cost_reduction(line);
    // Caller already verified lower.starts_with("equip") — strip 5-char prefix.
    // "equip" is always ASCII so byte length == char length.
    let rest = activation_line.get("equip".len()..)?;
    // Word-boundary guard: the keyword "equip" must terminate before a
    // non-keyword character. Permitted continuations: whitespace, em-dash,
    // hyphen, `{` (mana cost), or end-of-string. Anything else (e.g. 'm' from
    // "equipment", 'p' from "equipped" — though that's filtered earlier, 'a'
    // from a hypothetical "equipa") is a different word and must not match.
    if let Some(next) = rest.chars().next() {
        if !matches!(next, ' ' | '\t' | '\u{2014}' | '-' | '{' | '.') {
            return None;
        }
    }
    let rest = rest.trim();
    // Strip leading "—" or "- "
    let cost_text = rest
        .strip_prefix('—')
        .or_else(|| rest.strip_prefix('-'))
        .unwrap_or(rest)
        .trim();

    if cost_text.is_empty() {
        return None;
    }

    let (cost_text, constraints) = strip_activated_constraints(cost_text);
    let cost = parse_equip_cost(&cost_text);
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Attach {
            attachment: crate::types::ability::TargetFilter::SelfRef,
            target: crate::types::ability::TargetFilter::Typed(
                TypedFilter::creature().controller(crate::types::ability::ControllerRef::You),
            ),
        },
    )
    .cost(cost)
    .description(line.to_string())
    .sorcery_speed();
    if !constraints.restrictions.is_empty() {
        for restriction in constraints.restrictions {
            if !ability.activation_restrictions.contains(&restriction) {
                ability.activation_restrictions.push(restriction);
            }
        }
    }
    ability.cost_reduction = cost_reduction;
    Some(ability)
}

fn parse_equip_cost(cost_text: &str) -> AbilityCost {
    let cost = parse_oracle_cost(cost_text);
    if !matches!(cost, AbilityCost::Unimplemented { .. }) {
        return cost;
    }

    parse_first_mana_cost_in_text(cost_text)
        .map(|cost| AbilityCost::Mana { cost })
        .unwrap_or(cost)
}

fn parse_first_mana_cost_in_text(text: &str) -> Option<ManaCost> {
    let upper = text.to_ascii_uppercase();
    let (_, cost) = nom::sequence::preceded(
        take_until::<_, _, VerboseError<&str>>("{"),
        super::oracle_nom::primitives::parse_mana_cost,
    )
    .parse(upper.as_str())
    .ok()?;
    Some(cost)
}

fn split_trailing_self_cost_reduction(
    line: &str,
) -> (&str, Option<crate::types::ability::CostReduction>) {
    let lower = line.to_lowercase();
    let Some(((), reduction_text)) = nom_on_lower(line, &lower, |input| {
        value((), (take_until(". this ability costs "), tag(". "))).parse(input)
    }) else {
        return (line, None);
    };
    let Some(reduction) = try_parse_cost_reduction(reduction_text) else {
        return (line, None);
    };
    let activation_len = line.len() - ". ".len() - reduction_text.len();
    (line[..activation_len].trim(), Some(reduction))
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
                    apply_loyalty_restrictions(&mut def);
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
                apply_loyalty_restrictions(&mut def);
                return Some(def);
            }
        }
    }

    None
}

/// CR 606.3: A player may activate a loyalty ability only during a main phase
/// of their turn with an empty stack, and only if no player has previously
/// activated a loyalty ability of that permanent that turn. The planeswalker
/// activation path (`game::planeswalker::can_activate_loyalty`) already gates
/// this independently, but tagging the ability with `AsSorcery` +
/// `OnlyOnceEachTurn` + the display flag keeps parser output self-describing
/// and satisfies the shared invariant that every sorcery-speed activated
/// ability carries `ActivationRestriction::AsSorcery`.
fn apply_loyalty_restrictions(def: &mut AbilityDefinition) {
    // CR 606.3: "...only during a main phase of their turn when the stack is empty..."
    def.sorcery_speed = true;
    if !def
        .activation_restrictions
        .contains(&ActivationRestriction::AsSorcery)
    {
        def.activation_restrictions
            .push(ActivationRestriction::AsSorcery);
    }
    // CR 606.3: "...only if no player has previously activated a loyalty ability
    // of that permanent that turn."
    if !def
        .activation_restrictions
        .contains(&ActivationRestriction::OnlyOnceEachTurn)
    {
        def.activation_restrictions
            .push(ActivationRestriction::OnlyOnceEachTurn);
    }
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
        "return",
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

pub(super) fn strip_activated_constraints(text: &str) -> (String, ActivatedConstraintAst) {
    let mut remaining = text.trim().trim_end_matches('.').trim().to_string();
    let mut constraints = ActivatedConstraintAst::default();

    'parse_constraints: loop {
        let lower = remaining.to_lowercase();
        let tp = TextPair::new(&remaining, &lower);

        if let Some((before, after)) = tp.rsplit_around(" and only if ") {
            if !before.original.trim().is_empty() {
                let mut condition_text = after.original.trim().to_string();
                strip_once_per_turn_suffix(&mut condition_text, &mut constraints.restrictions);
                remaining = before
                    .original
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

        // CR 602.2: "Any player may activate this ability." — strip as a recognized
        // annotation. This appears as a trailing sentence on activated abilities.
        if let Some(prefix) = lower.strip_suffix("any player may activate this ability") {
            let end = remaining.len() - "any player may activate this ability".len();
            remaining = remaining[..end]
                .trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace())
                .to_string();
            constraints.any_player_may_activate = true;
            if prefix.trim().is_empty() {
                break;
            }
            continue;
        }

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
    if strip_condition_suffix(
        condition_text,
        " and only as a sorcery",
        ActivationRestriction::AsSorcery,
        restrictions,
    ) {
        strip_once_per_turn_suffix(condition_text, restrictions);
        return;
    }

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

fn strip_condition_suffix(
    condition_text: &mut String,
    suffix: &'static str,
    restriction: ActivationRestriction,
    restrictions: &mut Vec<ActivationRestriction>,
) -> bool {
    let lower = condition_text.to_lowercase();
    let suffix_len = match take_until::<_, _, VerboseError<&str>>(suffix).parse(lower.as_str()) {
        Ok((rest, _))
            if all_consuming(tag::<_, _, VerboseError<&str>>(suffix))
                .parse(rest)
                .is_ok() =>
        {
            suffix.len()
        }
        Err(_) => return false,
        _ => return false,
    };
    let stripped_len = condition_text.len() - suffix_len;
    *condition_text = condition_text[..stripped_len]
        .trim_end_matches(|c: char| c == ',' || c.is_whitespace()) // allow-noncombinator: structural punctuation cleanup after suffix parse
        .to_string();
    restrictions.push(restriction);
    true
}

/// Strip trailing "X can't be 0." / " X can't be 0." constraint annotations from Oracle text.
/// These are casting restrictions that annotate X-cost spells but are not themselves abilities.
fn strip_x_cant_be_zero_suffix(line: &str) -> String {
    let lower = line.to_lowercase();
    let trimmed = lower.trim_end_matches('.');
    // Standalone case: entire line is "X can't be 0"
    if trimmed == "x can't be 0" {
        return String::new();
    }
    // Suffix case: "... X can't be 0." at end of line
    for suffix in [". x can't be 0", " x can't be 0"] {
        if let Some(pos) = trimmed.rfind(suffix) {
            let mut result = line[..pos].to_string();
            // Preserve trailing period if we stripped at a sentence boundary
            if suffix.starts_with('.') {
                result.push('.');
            }
            return result.trim_end().to_string();
        }
    }
    line.to_string()
}

/// Primary nom-based dispatcher for Oracle text lines.
///
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

/// Parse an activated-ability effect chain with self-reference fallback.
///
/// Tries the raw text first so patterns that depend on the literal card name
/// (e.g. possessive forms like "Marwyn's power") keep working, then retries
/// with `~`-normalized text if the first pass left the result unimplemented
/// *or* emitted a `target-fallback` warning. The latter is the Metalhead
/// class: the effect parsed to a concrete variant but `parse_target` silently
/// fell back to `TargetFilter::Any` because the bare card-name wasn't
/// recognized as a self-reference. Warnings from the discarded pass are
/// dropped so they don't pollute coverage output.
pub(super) fn parse_activated_with_self_ref_fallback(
    effect_text: &str,
    card_name: &str,
) -> AbilityDefinition {
    let pre_warnings = take_warnings();

    let def = parse_effect_chain(effect_text, AbilityKind::Activated);
    let first_warnings = take_warnings();
    let first_has_target_fallback = first_warnings
        .iter()
        .any(|w| w.starts_with("target-fallback"));
    let first_clean = !has_unimplemented(&def) && !first_has_target_fallback;

    if first_clean {
        for w in pre_warnings {
            push_warning(w);
        }
        for w in first_warnings {
            push_warning(w);
        }
        return def;
    }

    let normalized = normalize_self_refs_for_static(effect_text, card_name);
    if normalized == effect_text {
        for w in pre_warnings {
            push_warning(w);
        }
        for w in first_warnings {
            push_warning(w);
        }
        return def;
    }

    let alt = parse_effect_chain(&normalized, AbilityKind::Activated);
    let alt_warnings = take_warnings();
    let alt_has_target_fallback = alt_warnings
        .iter()
        .any(|w| w.starts_with("target-fallback"));
    let alt_clean = !has_unimplemented(&alt) && !alt_has_target_fallback;

    for w in pre_warnings {
        push_warning(w);
    }
    if alt_clean {
        // Normalized pass is strictly better — keep only its (empty) warnings.
        for w in alt_warnings {
            push_warning(w);
        }
        alt
    } else {
        // Neither pass was clean; prefer the original result and preserve
        // first-pass diagnostics so the coverage dashboard reflects reality.
        for w in first_warnings {
            push_warning(w);
        }
        for w in alt_warnings {
            push_warning(w);
        }
        def
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AbilityCondition, AggregateFunction, Comparator, ContinuousModification, ControllerRef,
        FilterProp, ManaSpendRestriction, ModalSelectionCondition, ModalSelectionConstraint,
        ObjectScope, ParsedCondition, PlayerFilter, PlayerScope, PtValue, QuantityExpr,
        QuantityRef, ReplacementCondition, RoundingMode, SharedQuality, SharedQualityRelation,
        StaticCondition, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::keywords::{FlashbackCost, KeywordKind, WardCost};
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
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
    fn nonmodal_spell_contiguous_resolution_lines_chain_once() {
        let r = parse("Scry 1.\nDraw a card.", "Test Opt", &[], &["Instant"], &[]);

        assert_eq!(r.abilities.len(), 1);
        assert!(r.modal.is_none());
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
        let draw = r.abilities[0]
            .sub_ability
            .as_ref()
            .expect("draw should be chained after scry");
        assert!(matches!(
            *draw.effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
    }

    #[test]
    fn modal_spell_block_keeps_mode_branches_separate() {
        let r = parse(
            "Choose one —\n• Scry 1.\n• Draw a card.",
            "Test Charm",
            &[],
            &["Instant"],
            &[],
        );

        let modal = r.modal.expect("modal metadata should remain on spell face");
        assert_eq!(modal.mode_count, 2);
        assert_eq!(r.abilities.len(), 2);
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
        assert!(matches!(
            *r.abilities[1].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
    }

    #[test]
    fn non_spell_permanent_resolution_like_lines_do_not_merge() {
        let r = parse(
            "Target player draws a card.\nTarget player gains 3 life.",
            "Test Permanent",
            &[],
            &["Artifact"],
            &[],
        );

        assert_eq!(r.abilities.len(), 2);
        assert!(r.abilities[0].sub_ability.is_none());
        assert!(matches!(*r.abilities[0].effect, Effect::Draw { .. }));
        assert!(matches!(*r.abilities[1].effect, Effect::GainLife { .. }));
    }

    #[test]
    fn multani_cda_parses_total_cards_in_all_players_hands() {
        let r = parse(
            "Multani's power and toughness are each equal to the total number of cards in all players' hands.",
            "Multani, Maro-Sorcerer",
            &[],
            &["Creature"],
            &[],
        );

        assert!(
            r.abilities.is_empty(),
            "unexpected abilities: {:?}",
            r.abilities
        );
        assert_eq!(r.statics.len(), 1);
        let qty = QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: PlayerScope::AllPlayers {
                    aggregate: AggregateFunction::Sum,
                },
            },
        };
        assert_eq!(
            r.statics[0].modifications,
            vec![
                ContinuousModification::SetDynamicPower { value: qty.clone() },
                ContinuousModification::SetDynamicToughness { value: qty },
            ]
        );
    }

    #[test]
    fn kicker_and_or_line_sets_two_kicker_costs() {
        let r = parse(
            "Kicker {B} and/or {R}\nWhen ~ enters, if it was kicked twice, draw a card.",
            "Test Kicker",
            &[],
            &["Creature"],
            &[],
        );

        match r.additional_cost.expect("additional cost") {
            AdditionalCost::Kicker { costs, repeatable } => {
                assert!(!repeatable);
                assert_eq!(costs.len(), 2);
                assert!(matches!(
                    &costs[0],
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { shards, generic: 0 }
                    } if shards == &vec![ManaCostShard::Black]
                ));
                assert!(matches!(
                    &costs[1],
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { shards, generic: 0 }
                    } if shards == &vec![ManaCostShard::Red]
                ));
            }
            other => panic!("expected two-cost Kicker, got {other:?}"),
        }
    }

    #[test]
    fn multikicker_line_sets_repeatable_kicker_cost() {
        let r = parse(
            "Multikicker {1}{G}\nWhen ~ enters, draw a card.",
            "Test Multikicker",
            &[],
            &["Creature"],
            &[],
        );

        match r.additional_cost.expect("additional cost") {
            AdditionalCost::Kicker { costs, repeatable } => {
                assert!(repeatable);
                assert_eq!(costs.len(), 1);
                assert!(matches!(
                    &costs[0],
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { shards, generic: 1 }
                    } if shards == &vec![ManaCostShard::Green]
                ));
            }
            other => panic!("expected repeatable Kicker, got {other:?}"),
        }
    }

    #[test]
    fn non_mana_kicker_line_uses_oracle_cost_parser() {
        let r = parse(
            "Kicker—Sacrifice a land.\nWhen ~ enters, draw a card.",
            "Test Nonmana Kicker",
            &[],
            &["Creature"],
            &[],
        );

        match r.additional_cost.expect("additional cost") {
            AdditionalCost::Kicker { costs, repeatable } => {
                assert!(!repeatable);
                assert_eq!(costs.len(), 1);
                assert!(matches!(&costs[0], AbilityCost::Sacrifice { count: 1, .. }));
            }
            other => panic!("expected non-mana Kicker, got {other:?}"),
        }
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
    fn priest_of_titania_mana_ability_supported() {
        let r = parse(
            "{T}: Add {G} for each Elf on the battlefield.",
            "Priest of Titania",
            &[],
            &["Creature"],
            &["Elf", "Druid"],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
        assert!(matches!(*r.abilities[0].effect, Effect::Mana { .. }));
    }

    #[test]
    fn distinct_card_type_choose_wires_remainder_on_bottom() {
        use crate::types::ability::{ChooseFromZoneConstraint, LibraryPosition};
        let r = parse(
            "Flying, vigilance, deathtouch, lifelink\nWhen Atraxa enters, reveal the top ten cards of your library. For each card type, you may put a card of that type from among the revealed cards into your hand. Put the rest on the bottom of your library in a random order.",
            "Atraxa, Grand Unifier",
            &[
                Keyword::Flying,
                Keyword::Vigilance,
                Keyword::Deathtouch,
                Keyword::Lifelink,
            ],
            &["Creature"],
            &["Phyrexian", "Angel"],
        );
        assert_eq!(r.triggers.len(), 1);
        let trigger = &r.triggers[0];
        let def = trigger
            .execute
            .as_ref()
            .expect("trigger should have execute");
        assert!(
            !has_unimplemented(def),
            "ETB should not contain Unimplemented effects: {def:?}",
        );

        // Walk the effect chain: RevealTop → ChooseFromZone → ChangeZone(Library→Hand) → PutAtLibraryPosition(Bottom)
        let choose_def = def
            .sub_ability
            .as_ref()
            .expect("RevealTop should chain to ChooseFromZone");
        assert!(
            matches!(
                &*choose_def.effect,
                Effect::ChooseFromZone {
                    up_to: true,
                    constraint: Some(ChooseFromZoneConstraint::DistinctCardTypes { .. }),
                    ..
                }
            ),
            "Expected ChooseFromZone with DistinctCardTypes constraint, got {:?}",
            choose_def.effect,
        );

        let change_zone_def = choose_def
            .sub_ability
            .as_ref()
            .expect("ChooseFromZone should chain to ChangeZone(Library→Hand)");
        assert!(
            matches!(
                &*change_zone_def.effect,
                Effect::ChangeZone {
                    origin: Some(Zone::Library),
                    destination: Zone::Hand,
                    ..
                }
            ),
            "Expected ChangeZone(Library→Hand), got {:?}",
            change_zone_def.effect,
        );

        let bottom_def = change_zone_def
            .sub_ability
            .as_ref()
            .expect("ChangeZone should chain to PutAtLibraryPosition(Bottom) for unchosen cards");
        assert!(
            matches!(
                &*bottom_def.effect,
                Effect::PutAtLibraryPosition {
                    position: LibraryPosition::Bottom,
                    ..
                }
            ),
            "Expected PutAtLibraryPosition(Bottom), got {:?}",
            bottom_def.effect,
        );
    }

    #[test]
    fn blocked_wurms_beyond_first_pump_have_dynamic_quantity_no_warning() {
        for (name, pt, expected_power_factor) in
            [("Johtull Wurm", "-2/-1", -2), ("Jungle Wurm", "-1/-1", -1)]
        {
            let r = parse(
                &format!(
                    "Whenever this creature becomes blocked, it gets {pt} until end of turn for each creature blocking it beyond the first."
                ),
                name,
                &[],
                &["Creature"],
                &["Wurm"],
            );

            assert_eq!(r.triggers.len(), 1);
            assert_eq!(r.triggers[0].mode, TriggerMode::BecomesBlocked);
            assert!(
                r.parse_warnings
                    .iter()
                    .all(|warning| warning.split_whitespace().next() != Some("Swallow:DynamicQty")),
                "unexpected DynamicQty warning for {name}: {:?}",
                r.parse_warnings
            );
            let execute = r.triggers[0]
                .execute
                .as_ref()
                .expect("trigger should have execute");
            match execute.effect.as_ref() {
                Effect::Pump { power, .. } => match power {
                    PtValue::Quantity(QuantityExpr::Multiply { factor, inner }) => {
                        assert_eq!(*factor, expected_power_factor);
                        assert!(matches!(
                            inner.as_ref(),
                            QuantityExpr::Offset { offset: -1, .. }
                        ));
                    }
                    other => panic!("expected dynamic power multiplier, got {other:?}"),
                },
                other => panic!("expected Pump, got {other:?}"),
            }
        }
    }

    #[test]
    fn bhaal_myrkul_half_starting_life_static_has_typed_condition_no_dynamic_qty_warning() {
        for (name, subject) in [
            ("Bane, Lord of Darkness", "Bane"),
            ("Bhaal, Lord of Murder", "Bhaal"),
            ("Myrkul, Lord of Bones", "Myrkul"),
        ] {
            let r = parse(
                &format!(
                    "As long as your life total is less than or equal to half your starting life total, {subject} has indestructible."
                ),
                name,
                &[],
                &["Creature"],
                &[],
            );

            assert_eq!(r.statics.len(), 1, "{name}: {r:#?}");
            assert!(
                r.parse_warnings
                    .iter()
                    .all(|warning| warning.split_whitespace().next() != Some("Swallow:DynamicQty")),
                "unexpected DynamicQty warning for {name}: {:?}",
                r.parse_warnings
            );
            assert!(
                r.statics[0]
                    .modifications
                    .contains(&ContinuousModification::AddKeyword {
                        keyword: Keyword::Indestructible,
                    }),
                "expected indestructible grant for {name}: {:?}",
                r.statics[0].modifications
            );
            match r.statics[0]
                .condition
                .as_ref()
                .expect("expected static condition")
            {
                StaticCondition::QuantityComparison {
                    lhs:
                        QuantityExpr::Ref {
                            qty:
                                QuantityRef::LifeTotal {
                                    player: PlayerScope::Controller,
                                },
                        },
                    comparator: Comparator::LE,
                    rhs:
                        QuantityExpr::HalfRounded {
                            inner,
                            rounding: RoundingMode::Down,
                        },
                } => {
                    assert!(matches!(
                        inner.as_ref(),
                        QuantityExpr::Ref {
                            qty: QuantityRef::StartingLifeTotal
                        }
                    ));
                }
                other => panic!("expected typed half-starting-life comparison, got {other:?}"),
            }
        }
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
    fn parser_reaches_static_line_for_blocks_each_combat_if_able() {
        let r = parse(
            "This creature blocks each combat if able.",
            "Watchdog",
            &[],
            &["Creature"],
            &["Dog"],
        );
        assert_eq!(r.abilities.len(), 0);
        assert_eq!(r.statics.len(), 1);
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::MustBlock
        );
    }

    #[test]
    fn parser_reaches_static_line_for_other_goblins_attack_each_combat_if_able() {
        let r = parse(
            "Other Goblin creatures you control attack each combat if able.",
            "Goblin Assault",
            &[],
            &["Enchantment"],
            &[],
        );
        assert_eq!(r.abilities.len(), 0, "{r:#?}");
        assert_eq!(r.statics.len(), 1, "{r:#?}");
        assert_eq!(
            r.statics[0].mode,
            crate::types::statics::StaticMode::MustAttack
        );
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
    fn commander_permission_line_is_deck_construction_text() {
        let r = parse(
            "Teferi, Temporal Archmage can be your commander.",
            "Teferi, Temporal Archmage",
            &[],
            &["Planeswalker"],
            &["Teferi"],
        );

        assert!(r.abilities.is_empty());
        assert!(r.triggers.is_empty());
        assert!(r.statics.is_empty());
        assert!(r.replacements.is_empty());
    }

    #[test]
    fn oracle_text_allows_commander_uses_commander_permission_parser() {
        assert!(oracle_text_allows_commander(
            "Teferi, Temporal Archmage can be your commander.",
            "Teferi, Temporal Archmage",
        ));
        assert!(oracle_text_allows_commander(
            "Spell commander (This card can be your commander. In Limited, it can partner like other monocolored legends.)",
            "Clear, the Mind",
        ));
        assert!(!oracle_text_allows_commander(
            "Teferi, Temporal Archmage can't be your commander.",
            "Teferi, Temporal Archmage",
        ));
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
        let Effect::Draw { count, target, .. } = &*r.abilities[0].effect else {
            panic!("expected Effect::Draw, got {:?}", r.abilities[0].effect);
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
        // CR 601.2c: "Target player draws ..." selects a player target during
        // spell announcement — the parsed Draw must carry a Player filter, not
        // Controller (which would always draw for the caster).
        assert!(
            matches!(target, TargetFilter::Player),
            "expected TargetFilter::Player for 'Target player draws a card.', got {target:?}",
        );
    }

    #[test]
    fn ashlings_command_modal_target_player_draws_carries_player_filter() {
        // CR 601.2c + CR 700.2: Each "target player" mode-clause of a modal
        // spell is an independent target chosen during spell announcement.
        // Mode 2 ("Target player draws two cards") MUST surface a Player
        // target on the parsed Draw effect so `collect_target_slots` emits
        // an independent slot per Draw mode (otherwise the caster always draws).
        let r = parse(
            "Choose two —\n\
             • Create a token that's a copy of target Elemental you control.\n\
             • Target player draws two cards.\n\
             • Ashling's Command deals 2 damage to each creature target player controls.\n\
             • Target player creates two Treasure tokens.",
            "Ashling's Command",
            &[],
            &["Instant"],
            &[],
        );
        // Modal spell exposes one ability with chained sub_ability per mode.
        // Find the Draw clause anywhere in the chain and assert its target.
        fn find_draw(
            ab: &crate::types::ability::AbilityDefinition,
        ) -> Option<&crate::types::ability::TargetFilter> {
            if let Effect::Draw { target, .. } = &*ab.effect {
                return Some(target);
            }
            ab.sub_ability.as_deref().and_then(find_draw)
        }
        let mut draw_target = None;
        for ab in r.abilities.iter() {
            if let Some(t) = find_draw(ab) {
                draw_target = Some(t);
                break;
            }
        }
        let target = draw_target.expect("expected a Draw effect somewhere in the modal chain");
        assert!(
            matches!(target, TargetFilter::Player),
            "Mode 2 Draw must carry TargetFilter::Player so each modal mode \
             surfaces an independent target slot, got {target:?}",
        );
    }

    #[test]
    fn ashlings_command_modal_target_player_creates_tokens_carries_player_filter() {
        // CR 111.2 + CR 601.2c: Each "Target player creates ..." mode-clause
        // of a modal spell is an independent target chosen during spell
        // announcement. Mode 4 of Ashling's Command MUST surface a Player
        // filter on the parsed Token effect's `owner` field so
        // `collect_target_slots` emits an independent slot per token mode
        // (otherwise the caster always creates the tokens).
        let r = parse(
            "Choose two —\n\
             • Create a token that's a copy of target Elemental you control.\n\
             • Target player draws two cards.\n\
             • Ashling's Command deals 2 damage to each creature target player controls.\n\
             • Target player creates two Treasure tokens.",
            "Ashling's Command",
            &[],
            &["Instant"],
            &[],
        );
        fn find_token(
            ab: &crate::types::ability::AbilityDefinition,
        ) -> Option<&crate::types::ability::TargetFilter> {
            if let Effect::Token { owner, .. } = &*ab.effect {
                return Some(owner);
            }
            ab.sub_ability.as_deref().and_then(find_token)
        }
        // Find a Token effect whose owner is `Player` (mode 4). Mode 1 also
        // creates a token but its owner is `Controller`, so we keep searching.
        let mut owner_target = None;
        for ab in r.abilities.iter() {
            // Walk the entire chain, collecting any Player-owner Token we see.
            let mut cur: Option<&crate::types::ability::AbilityDefinition> = Some(ab);
            while let Some(node) = cur {
                if let Some(t) = find_token(node) {
                    if matches!(t, TargetFilter::Player) {
                        owner_target = Some(t);
                        break;
                    }
                }
                cur = node.sub_ability.as_deref();
            }
            if owner_target.is_some() {
                break;
            }
        }
        let target = owner_target
            .expect("expected a Token effect with TargetFilter::Player owner in the modal chain");
        assert!(
            matches!(target, TargetFilter::Player),
            "Mode 4 Token must carry owner=TargetFilter::Player so each modal \
             mode surfaces an independent target slot, got {target:?}",
        );
    }

    #[test]
    fn target_player_scrys_carries_player_filter() {
        // CR 701.22a + CR 601.2c: "Target player scrys N" surfaces an
        // independent player target on the parsed Scry effect — the resolver
        // routes the scry to the chosen player, not the spell's controller.
        let r = parse(
            "Target player scries 2.",
            "Test Permanent",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let Effect::Scry { count, target } = &*r.abilities[0].effect else {
            panic!("expected Effect::Scry, got {:?}", r.abilities[0].effect);
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
        assert!(
            matches!(target, TargetFilter::Player),
            "expected TargetFilter::Player for 'Target player scries 2.', got {target:?}",
        );
    }

    #[test]
    fn target_player_surveils_carries_player_filter() {
        // CR 701.25a + CR 601.2c: "Target player surveils N" surfaces an
        // independent player target on the parsed Surveil effect — the
        // resolver routes the surveil to the chosen player, not the spell's
        // controller. (Mirrors the Draw + Scry tests above.)
        let r = parse(
            "Target player surveils 2.",
            "Test Permanent",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let Effect::Surveil { count, target } = &*r.abilities[0].effect else {
            panic!("expected Effect::Surveil, got {:?}", r.abilities[0].effect);
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 2 });
        assert!(
            matches!(target, TargetFilter::Player),
            "expected TargetFilter::Player for 'Target player surveils 2.', got {target:?}",
        );
    }

    #[test]
    fn target_player_mills_carries_player_filter() {
        // CR 701.13a + CR 601.2c: "Target player mills N" surfaces an
        // independent player target on the parsed Mill effect — the resolver
        // routes the mill to the chosen player, not the spell's controller.
        // Mirror coverage for the Scry/Surveil tests above so the conjugated
        // verb path ("mills" via y/s normalization) is pinned for regression.
        let r = parse(
            "Target player mills 3.",
            "Test Permanent",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let Effect::Mill { count, target, .. } = &*r.abilities[0].effect else {
            panic!("expected Effect::Mill, got {:?}", r.abilities[0].effect);
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
        assert!(
            matches!(target, TargetFilter::Player),
            "expected TargetFilter::Player for 'Target player mills 3.', got {target:?}",
        );
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
                count: QuantityExpr::Fixed { value: 1 },
                ..
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
    fn incubate_parses_as_effect() {
        let r = parse(
            "When this creature enters, incubate 3.",
            "Converter Beast",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        let trigger_def = r.triggers[0].execute.as_ref().unwrap();
        assert!(
            matches!(&*trigger_def.effect, crate::types::ability::Effect::Incubate { count }
                if matches!(count, crate::types::ability::QuantityExpr::Fixed { value: 3 })),
            "Expected Incubate {{ count: Fixed(3) }}, got {:?}",
            trigger_def.effect
        );
    }

    #[test]
    fn attack_this_turn_if_able_parses_as_effect() {
        let r = parse(
            "Target creature attacks this turn if able.\nDraw a card.",
            "Boiling Blood",
            &[],
            &["Instant"],
            &[],
        );
        assert!(!r.abilities.is_empty());
        assert!(
            matches!(
                &*r.abilities[0].effect,
                crate::types::ability::Effect::GenericEffect {
                    static_abilities,
                    ..
                } if !static_abilities.is_empty()
                    && static_abilities[0].mode == crate::types::statics::StaticMode::MustAttack
            ),
            "Expected GenericEffect with MustAttack, got {:?}",
            r.abilities[0].effect
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
                count: QuantityExpr::Fixed { value: 1 },
                ..
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
    fn permanent_casting_option_parses_flash_permission_with_extra_cost() {
        let r = parse(
            "You may cast this spell as though it had flash if you pay {2} more to cast it.\nWhen this creature enters, draw a card.",
            "Example Ambusher",
            &[],
            &["Creature"],
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
        assert_eq!(r.triggers.len(), 1);
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
    fn spell_resolution_free_cast_from_hand_is_effect_not_static() {
        let r = parse(
            "Return up to three target artifacts and/or creatures to their owners' hands.\nYou may cast a spell with mana value 4 or less from your hand without paying its mana cost.",
            "Baral's Expertise",
            &[],
            &["Sorcery"],
            &[],
        );

        assert_eq!(r.statics.len(), 0);
        assert_eq!(r.abilities.len(), 1);
        let cast = r.abilities[0].sub_ability.as_ref().unwrap_or_else(|| {
            panic!(
                "free cast instruction should be chained after bounce, got {:?}",
                r.abilities[0]
            )
        });
        assert!(cast.optional);
        match &*cast.effect {
            Effect::CastFromZone {
                target: TargetFilter::Typed(filter),
                without_paying_mana_cost: true,
                mode: crate::types::ability::CardPlayMode::Cast,
                ..
            } => {
                assert_eq!(filter.type_filters, vec![TypeFilter::Card]);
                assert_eq!(
                    filter.controller,
                    Some(crate::types::ability::ControllerRef::You)
                );
                assert!(filter
                    .properties
                    .iter()
                    .any(|prop| matches!(prop, FilterProp::InZone { zone: Zone::Hand })));
                assert!(filter.properties.iter().any(|prop| matches!(
                    prop,
                    FilterProp::Cmc {
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 4 },
                    }
                )));
            }
            effect => panic!("expected optional CastFromZone, got {effect:?}"),
        }
        assert_eq!(r.parse_warnings, Vec::<String>::new());
    }

    #[test]
    fn permanent_free_cast_from_hand_remains_static_permission() {
        let r = parse(
            "You may cast spells from your hand without paying their mana costs.",
            "Omniscience",
            &[],
            &["Enchantment"],
            &[],
        );

        assert_eq!(r.abilities.len(), 0);
        assert_eq!(r.statics.len(), 1);
        assert!(matches!(
            r.statics[0].mode,
            StaticMode::CastFromHandFree { .. }
        ));
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
        for ab in r.abilities.iter() {
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
    fn parses_return_forest_cost_untap_activated_ability() {
        let r = parse(
            "Return a Forest you control to its owner's hand: Untap target creature. Activate only once each turn.",
            "Quirion Ranger",
            &[],
            &["Creature"],
            &["Elf", "Ranger"],
        );

        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert_eq!(ability.kind, AbilityKind::Activated);
        assert!(matches!(*ability.effect, Effect::Untap { .. }));
        assert!(ability
            .activation_restrictions
            .iter()
            .any(|restriction| matches!(restriction, ActivationRestriction::OnlyOnceEachTurn)));
        match ability.cost.as_ref() {
            Some(AbilityCost::ReturnToHand {
                count,
                filter: Some(TargetFilter::Typed(filter)),
            }) => {
                assert_eq!(*count, 1);
                assert_eq!(filter.get_subtype(), Some("Forest"));
            }
            other => panic!("expected Forest ReturnToHand cost, got {other:?}"),
        }
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
    fn parses_activate_only_if_condition_and_only_as_sorcery() {
        let r = parse(
            "{2}{G}{G}: Return this card from your graveyard to the battlefield. Activate only if there are four or more card types among cards in your graveyard and only as a sorcery.",
            "Delirium Test",
            &[],
            &["Creature"],
            &[],
        );

        assert_eq!(r.abilities.len(), 1);
        let restrictions = &r.abilities[0].activation_restrictions;
        assert!(restrictions.contains(&ActivationRestriction::AsSorcery));
        assert!(restrictions.iter().any(|restriction| matches!(
            restriction,
            ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::ZoneCardTypeCountAtLeast {
                    zone: Zone::Graveyard,
                    count: 4
                })
            }
        )));
        assert_eq!(r.parse_warnings, Vec::<String>::new());
    }

    #[test]
    fn parses_activate_only_timing_and_only_if_condition() {
        let r = parse(
            "{1}{B}: Return this card from your graveyard to your hand. Activate only during your turn and only if an opponent lost life this turn.",
            "Gutterbones",
            &[],
            &["Creature"],
            &[],
        );
        let restrictions = &r.abilities[0].activation_restrictions;
        assert!(restrictions.contains(&ActivationRestriction::DuringYourTurn));
        assert!(restrictions.iter().any(|restriction| matches!(
            restriction,
            ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::PlayerCountAtLeast {
                    filter: PlayerFilter::OpponentLostLife,
                    minimum: 1,
                })
            }
        )));
        assert!(r
            .parse_warnings
            .iter()
            .all(|warning| warning.split_whitespace().next() != Some("Swallow:Condition_If")));
    }

    #[test]
    fn parses_activate_only_as_sorcery_and_only_if_hand_size_condition() {
        let r = parse(
            "{2}{B}: Return this card from your graveyard to the battlefield. Activate only as a sorcery and only if you have one or fewer cards in hand.",
            "Dread Wanderer",
            &[],
            &["Creature"],
            &[],
        );
        let restrictions = &r.abilities[0].activation_restrictions;
        assert!(restrictions.contains(&ActivationRestriction::AsSorcery));
        assert!(restrictions.iter().any(|restriction| matches!(
            restriction,
            ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::HandSizeOneOf { counts })
            } if counts == &vec![0, 1]
        )));
        assert!(r
            .parse_warnings
            .iter()
            .all(|warning| warning.split_whitespace().next() != Some("Swallow:Condition_If")));
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
    fn end_to_end_suspend_sorcery_no_unimplemented() {
        // CR 702.62a: "Suspend N—{cost}" on a sorcery must not produce Unimplemented.
        // Ancestral Vision: "Suspend 4—{U}\nTarget player draws three cards."
        let r = parse_with_keyword_names(
            "Suspend 4\u{2014}{U}\nTarget player draws three cards.",
            "Ancestral Vision",
            &["suspend"],
            &["Sorcery"],
            &[],
        );
        let has_unimplemented = r.abilities.iter().any(|a| {
            matches!(
                *a.effect,
                crate::types::ability::Effect::Unimplemented { .. }
            )
        });
        assert!(
            !has_unimplemented,
            "Suspend keyword line on sorcery should not produce Unimplemented"
        );
        // Should have extracted the parameterized Suspend keyword
        let suspend_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::Suspend { .. }));
        assert!(suspend_kw.is_some(), "Should extract Suspend keyword");
        if let Some(Keyword::Suspend { count, .. }) = suspend_kw {
            assert_eq!(*count, 4);
        }
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
        assert_eq!(
            modal.constraints,
            vec![ModalSelectionConstraint::ConditionalMaxChoices {
                condition: ModalSelectionCondition::ControlsCommander,
                max_choices: 2,
                otherwise_max_choices: 1,
            }]
        );
        assert_eq!(r.parse_warnings, Vec::<String>::new());
        assert!(matches!(
            *r.abilities[0].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                ..
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
                count: QuantityExpr::Fixed { value: 1 },
                ..
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
                count: QuantityExpr::Fixed { value: 1 },
                ..
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
                amount: QuantityExpr::Fixed { value: 15 },
                ..
            }
        ));
    }

    // CR 702.xxx: Prepare (Strixhaven) — Biblioplex Tomekeeper's ETB is a
    // modal trigger whose branches invoke the `becomes prepared` / `becomes
    // unprepared` imperatives. The modal-branch builder must route each
    // branch body through the same effect-chain parser that recognizes these
    // imperatives at the top level. Assign when WotC publishes SOS CR update.
    #[test]
    fn biblioplex_modal_etb_routes_becomes_prepared_branches() {
        let r = parse(
            "When this creature enters, choose up to one —\n• Target creature becomes prepared. (Only creatures with prepare spells can become prepared.)\n• Target creature becomes unprepared.",
            "Biblioplex Tomekeeper",
            &[],
            &["Creature"],
            &[],
        );
        assert!(r.abilities.is_empty());
        assert_eq!(r.triggers.len(), 1);

        let execute = r.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        let modal = execute.modal.as_ref().expect("execute should be modal");
        assert_eq!(modal.mode_count, 2);
        assert_eq!(execute.mode_abilities.len(), 2);

        // First branch: Target creature becomes prepared.
        assert!(matches!(
            *execute.mode_abilities[0].effect,
            Effect::BecomePrepared { .. }
        ));
        // Second branch: Target creature becomes unprepared.
        assert!(matches!(
            *execute.mode_abilities[1].effect,
            Effect::BecomeUnprepared { .. }
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
                count: QuantityExpr::Fixed { value: 1 },
                ..
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
                crate::types::counter::CounterType::Lore
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
                    count: QuantityExpr::Ref { .. },
                    ..
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
                count: QuantityExpr::Fixed { value: 1 },
                ..
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

    #[test]
    fn spell_prevention_keeps_preceding_dynamic_gain_life() {
        use crate::types::ability::{PreventionAmount, QuantityExpr, QuantityRef};

        let parsed = parse(
            "You gain 1 life for each creature on the battlefield. Prevent all combat damage that would be dealt this turn.",
            "Blunt the Assault",
            &[],
            &["Instant"],
            &[],
        );

        assert!(
            parsed.replacements.is_empty(),
            "spell prevention should parse as resolving effect, got {:?}",
            parsed.replacements
        );
        assert_eq!(parsed.abilities.len(), 1);
        let ability = &parsed.abilities[0];
        match &*ability.effect {
            Effect::GainLife {
                amount:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { .. },
                    },
                ..
            } => {}
            other => panic!("expected dynamic GainLife, got {other:?}"),
        }
        let prevention = ability
            .sub_ability
            .as_ref()
            .expect("expected prevention follow-up");
        assert!(matches!(
            &*prevention.effect,
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                ..
            }
        ));
        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| warning.split_whitespace().next() != Some("Swallow:DynamicQty")),
            "unexpected dynamic quantity warning: {:?}",
            parsed.parse_warnings
        );
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

    // ── Cumulative upkeep (CR 702.24) ──

    #[test]
    fn parse_cumulative_upkeep_mana_cost() {
        // CR 702.24: Mana-only cumulative upkeep — space-separated format.
        let r = parse(
            "Cumulative upkeep {1} (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)",
            "Mystic Remora",
            &[],
            &["Enchantment"],
            &[],
        );
        let cu_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::CumulativeUpkeep(_)));
        assert!(cu_kw.is_some(), "CumulativeUpkeep keyword not extracted");
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(cost) => assert_eq!(cost, "{1}"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_cumulative_upkeep_life_payment() {
        // CR 702.24: Non-mana cost with em-dash separator.
        let r = parse(
            "Cumulative upkeep\u{2014}Pay 2 life. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)",
            "Inner Sanctum",
            &[],
            &["Enchantment"],
            &[],
        );
        let cu_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::CumulativeUpkeep(_)));
        assert!(cu_kw.is_some(), "CumulativeUpkeep keyword not extracted");
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(cost) => assert_eq!(cost, "Pay 2 life"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_cumulative_upkeep_sacrifice() {
        // CR 702.24: Sacrifice cost.
        let r = parse(
            "Cumulative upkeep\u{2014}Sacrifice a land. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)",
            "Polar Kraken",
            &[],
            &["Creature"],
            &[],
        );
        let cu_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::CumulativeUpkeep(_)));
        assert!(cu_kw.is_some(), "CumulativeUpkeep keyword not extracted");
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(cost) => assert_eq!(cost, "Sacrifice a land"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_cumulative_upkeep_or_mana() {
        // CR 702.24: "{G} or {W}" — alternative mana cost.
        let r = parse(
            "Cumulative upkeep {G} or {W} (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)",
            "Elephant Grass",
            &[],
            &["Enchantment"],
            &[],
        );
        let cu_kw = r
            .extracted_keywords
            .iter()
            .find(|k| matches!(k, Keyword::CumulativeUpkeep(_)));
        assert!(cu_kw.is_some(), "CumulativeUpkeep keyword not extracted");
        match cu_kw.unwrap() {
            Keyword::CumulativeUpkeep(cost) => assert_eq!(cost, "{G} or {W}"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn earthbend_chain_defaults_target() {
        use crate::parser::oracle_effect::parse_effect_chain;

        // Single chunk: "Earthbend 3" — passes through imperative pipeline
        let simple = parse_effect_chain("Earthbend 3", crate::types::ability::AbilityKind::Spell);
        match &*simple.effect {
            Effect::Animate { target, .. } => {
                assert_eq!(
                    simple.duration,
                    Some(crate::types::ability::Duration::Permanent)
                );
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
            Effect::Animate { target, .. } => {
                assert_eq!(
                    full.duration,
                    Some(crate::types::ability::Duration::Permanent)
                );
                assert!(
                    matches!(target, TargetFilter::Typed(tf) if tf.type_filters.contains(&crate::types::ability::TypeFilter::Land)),
                    "chain earthbend should target land, got {target:?}"
                );
            }
            other => panic!("Expected Animate for chain earthbend, got {other:?}"),
        }
    }

    /// CR 122.1: Toph's "earthbend X, where X is the number of experience
    /// counters you have" must thread the dynamic count through to PutCounter,
    /// not collapse to Fixed { value: 0 }. Walks the parsed chain:
    /// Animate → PutCounter (count = PlayerCounter Experience Controller) →
    /// CreateDelayedTrigger.
    #[test]
    fn earthbend_x_where_x_is_experience_counters() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{CountScope, QuantityExpr, QuantityRef};
        use crate::types::player::PlayerCounterKind;

        let def = parse_effect_chain(
            "Earthbend X, where X is the number of experience counters you have.",
            crate::types::ability::AbilityKind::Spell,
        );
        assert!(
            matches!(&*def.effect, Effect::Animate { .. }),
            "outer effect should be Animate, got {:?}",
            def.effect
        );

        let put_counters = def
            .sub_ability
            .as_deref()
            .expect("Animate should have PutCounter sub_ability");
        match &*put_counters.effect {
            Effect::PutCounter {
                counter_type,
                count,
                ..
            } => {
                assert_eq!(counter_type, "P1P1");
                assert_eq!(
                    *count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::PlayerCounter {
                            kind: PlayerCounterKind::Experience,
                            scope: CountScope::Controller,
                        },
                    },
                    "Toph's PutCounter count should be a typed PlayerCounter ref, not Fixed 0"
                );
            }
            other => panic!("Expected PutCounter, got {other:?}"),
        }

        let delayed = put_counters
            .sub_ability
            .as_deref()
            .expect("PutCounter should chain into the delayed return trigger");
        assert!(
            matches!(&*delayed.effect, Effect::CreateDelayedTrigger { .. }),
            "expected CreateDelayedTrigger, got {:?}",
            delayed.effect,
        );
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
        // First effect is Animate (earthbend); the earthbend clause builds a deeper chain
        // (PutCounter → CreateDelayedTrigger → RegisterBending) before the "Then" search.
        // Walk the chain to find SearchLibrary.
        let mut cursor = def2.sub_ability.as_deref();
        while let Some(node) = cursor {
            if matches!(&*node.effect, Effect::SearchLibrary { .. }) {
                break;
            }
            cursor = node.sub_ability.as_deref();
        }
        let search = cursor.expect("should find SearchLibrary in earthbend chain");
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
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(counter_type) } },
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
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(counter_type) } },
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
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(counter_type) } },
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
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(counter_type) } },
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
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(counter_type) } },
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
                    lhs: QuantityExpr::Ref { qty: QuantityRef::CountersOn { scope: ObjectScope::Source, counter_type: Some(counter_type) } },
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

    /// CR 207.2c (Strive) + CR 115.1d ("any number of") + CR 707.2 (CopyTokenOf) +
    /// CR 702.10 (Haste) + CR 603.7 (delayed trigger): Twinflame's full parse —
    /// multi-target {min:0,max:None}, per-target CopyTokenOf{ParentTarget,
    /// extra_keywords:[Haste]}, delayed exile of "those tokens" with
    /// uses_tracked_set=true.
    #[test]
    fn twinflame_full_parse() {
        use crate::types::ability::{Effect, MultiTargetSpec, TargetFilter};
        use crate::types::keywords::Keyword;

        let r = parse(
            "Strive \u{2014} This spell costs {2}{R} more to cast for each target beyond the first.\nChoose any number of target creatures you control. For each of them, create a token that's a copy of that creature, except it has haste. Exile those tokens at the beginning of the next end step.",
            "Twinflame",
            &[],
            &["Sorcery"],
            &[],
        );

        // Strive cost extracted.
        let strive = r.strive_cost.as_ref().expect("strive_cost set");
        assert_eq!(strive.mana_value(), 3);

        // One spell ability with multi_target.
        assert_eq!(r.abilities.len(), 1, "expected single spell ability");
        let ab = &r.abilities[0];
        assert_eq!(
            ab.multi_target,
            Some(MultiTargetSpec::unlimited(0)),
            "expected any-number multi_target"
        );

        // Walk the chain: TargetOnly(creature) → CopyTokenOf → CreateDelayedTrigger.
        let copy = ab.sub_ability.as_ref().expect("CopyTokenOf sub-ability");
        match &*copy.effect {
            Effect::CopyTokenOf {
                target,
                extra_keywords,
                ..
            } => {
                assert!(matches!(target, TargetFilter::ParentTarget));
                assert_eq!(extra_keywords, &vec![Keyword::Haste]);
            }
            other => panic!("expected CopyTokenOf, got {other:?}"),
        }

        let delayed = copy
            .sub_ability
            .as_ref()
            .expect("CreateDelayedTrigger sub-ability");
        match &*delayed.effect {
            Effect::CreateDelayedTrigger {
                uses_tracked_set, ..
            } => assert!(
                *uses_tracked_set,
                "'those tokens' must mark uses_tracked_set=true"
            ),
            other => panic!("expected CreateDelayedTrigger, got {other:?}"),
        }
    }

    // ── Mana spend restriction extensions ─────────────────────────────

    #[test]
    fn mana_spend_restriction_activate_only() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result = parse_mana_spend_restriction("spend this mana only to activate abilities");
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::ActivateOnly)
        );
    }

    #[test]
    fn mana_spend_restriction_noncreature_spells() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result =
            parse_mana_spend_restriction("spend this mana only to cast noncreature spells");
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellType("Noncreature".to_string()))
        );
    }

    #[test]
    fn mana_spend_restriction_x_cost_only() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result = parse_mana_spend_restriction("spend this mana only on costs that include {x}");
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::XCostOnly)
        );
    }

    #[test]
    fn mana_spend_restriction_instant_or_sorcery() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::ability::ManaSpendRestriction;
        let result =
            parse_mana_spend_restriction("spend this mana only to cast instant or sorcery spells");
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellType(
                "Instant or Sorcery".to_string()
            ))
        );
    }

    #[test]
    fn mana_spend_restriction_flashback_spells() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        let result =
            parse_mana_spend_restriction("spend this mana only to cast spells with flashback");
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellWithKeywordKind(
                KeywordKind::Flashback,
            ))
        );
    }

    #[test]
    fn mana_spend_restriction_flashback_spells_from_graveyard() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast spells with flashback from a graveyard",
        );
        assert_eq!(
            result.map(|(r, _)| r),
            Some(ManaSpendRestriction::SpellWithKeywordKindFromZone {
                kind: KeywordKind::Flashback,
                zone: Zone::Graveyard,
            })
        );
    }

    #[test]
    fn mana_spend_restriction_chosen_type_cant_be_countered() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::mana::ManaSpellGrant;
        // Cavern of Souls pattern
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast a creature spell of the chosen type, and that spell can't be countered",
        );
        let (restriction, grants) = result.expect("should parse");
        assert_eq!(restriction, ManaSpendRestriction::ChosenCreatureType);
        assert_eq!(grants, vec![ManaSpellGrant::CantBeCountered]);
    }

    #[test]
    fn mana_spend_restriction_legendary_cant_be_countered() {
        use crate::parser::oracle_effect::mana::parse_mana_spend_restriction;
        use crate::types::mana::ManaSpellGrant;
        // Delighted Halfling pattern
        let result = parse_mana_spend_restriction(
            "spend this mana only to cast a legendary spell, and that spell can't be countered",
        );
        let (restriction, grants) = result.expect("should parse");
        assert_eq!(
            restriction,
            ManaSpendRestriction::SpellType("Legendary".to_string())
        );
        assert_eq!(grants, vec![ManaSpellGrant::CantBeCountered]);
    }

    #[test]
    fn top_level_static_flashback_grant_stays_on_graveyard_cards() {
        let result = parse(
            "Each instant and sorcery card in your graveyard has flashback.\nThe flashback cost is equal to that card's mana cost.",
            "Lier, Disciple of the Drowned",
            &[],
            &["Creature"],
            &["Human", "Wizard"],
        );
        assert!(result.extracted_keywords.is_empty());
        assert_eq!(result.statics.len(), 1);
        let static_def = &result.statics[0];
        match static_def.affected.as_ref() {
            Some(TargetFilter::Or { filters }) => {
                assert_eq!(filters.len(), 2);
                for filter in filters {
                    let TargetFilter::Typed(tf) = filter else {
                        panic!("expected typed branch, got {:?}", filter);
                    };
                    assert_eq!(
                        tf.controller,
                        Some(crate::types::ability::ControllerRef::You)
                    );
                    assert!(
                        tf.properties.contains(&FilterProp::InZone {
                            zone: Zone::Graveyard
                        }),
                        "missing graveyard filter: {:?}",
                        tf.properties
                    );
                    assert!(
                        tf.type_filters.contains(&TypeFilter::Instant)
                            || tf.type_filters.contains(&TypeFilter::Sorcery)
                    );
                }
            }
            other => panic!("expected typed affected filter, got {:?}", other),
        }
        assert!(
            static_def
                .modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
                }),
            "missing flashback grant: {:?}",
            static_def.modifications
        );
    }

    #[test]
    fn same_line_static_flashback_grant_stays_on_graveyard_cards() {
        let result = parse(
            "Spells can't be countered.\nEach instant and sorcery card in your graveyard has flashback. The flashback cost is equal to that card's mana cost.",
            "Lier, Disciple of the Drowned",
            &[],
            &["Creature"],
            &["Human", "Wizard"],
        );
        assert!(result.extracted_keywords.is_empty());
        assert_eq!(result.statics.len(), 2);
        assert!(result.statics.iter().any(|static_def| {
            static_def
                .modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::SelfManaCost)),
                })
        }));
    }

    #[test]
    fn top_level_static_escape_grant_stays_on_graveyard_cards() {
        let result = parse(
            "Each nonland card in your graveyard has escape.\nThe escape cost is equal to the card's mana cost plus exile three other cards from your graveyard.",
            "Underworld Breach",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(result.extracted_keywords.is_empty());
        assert_eq!(result.statics.len(), 1);
        let static_def = &result.statics[0];
        let TargetFilter::Typed(tf) = static_def
            .affected
            .as_ref()
            .expect("expected affected filter")
        else {
            panic!("expected typed affected filter");
        };
        assert_eq!(
            tf.controller,
            Some(crate::types::ability::ControllerRef::You)
        );
        assert!(
            tf.properties.contains(&FilterProp::InZone {
                zone: Zone::Graveyard
            }),
            "missing graveyard filter: {:?}",
            tf.properties
        );
        assert!(
            static_def
                .modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Escape {
                        cost: ManaCost::SelfManaCost,
                        exile_count: 3,
                    },
                }),
            "missing escape grant: {:?}",
            static_def.modifications
        );
    }

    #[test]
    fn same_line_static_escape_grant_stays_on_graveyard_cards() {
        let result = parse(
            "Each nonland card in your graveyard has escape. The escape cost is equal to the card's mana cost plus exile three other cards from your graveyard.",
            "Underworld Breach",
            &[],
            &["Enchantment"],
            &[],
        );
        assert!(result.extracted_keywords.is_empty());
        assert_eq!(result.statics.len(), 1);
        assert!(result.statics.iter().any(|static_def| {
            static_def
                .modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Escape {
                        cost: ManaCost::SelfManaCost,
                        exile_count: 3,
                    },
                })
        }));
    }

    #[test]
    fn helper_parses_same_line_escape_grant_continuation() {
        let static_def = try_parse_graveyard_keyword_static_with_continuation(
            "Each nonland card in your graveyard has escape. The escape cost is equal to the card's mana cost plus exile three other cards from your graveyard.",
        )
        .expect("helper should parse same-line escape continuation");
        assert!(
            static_def
                .modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Escape {
                        cost: ManaCost::SelfManaCost,
                        exile_count: 3,
                    },
                }),
            "missing escape grant: {:?}",
            static_def.modifications
        );
    }

    #[test]
    fn escape_continuation_parser_accepts_self_mana_cost_clause() {
        let keyword = parse_graveyard_keyword_continuation(
            "The escape cost is equal to the card's mana cost plus exile three other cards from your graveyard.",
            GraveyardGrantedKeywordKind::Escape,
        )
        .expect("continuation should parse");
        assert_eq!(
            keyword,
            Keyword::Escape {
                cost: ManaCost::SelfManaCost,
                exile_count: 3,
            }
        );
    }

    #[test]
    fn escape_continuation_parser_rejects_trailing_text() {
        let keyword = parse_graveyard_keyword_continuation(
            "The escape cost is equal to the card's mana cost plus exile three other cards from your graveyard until end of turn.",
            GraveyardGrantedKeywordKind::Escape,
        );
        assert!(
            keyword.is_none(),
            "trailing text should reject continuation"
        );
    }

    #[test]
    fn viral_spawning_corrupted_line_parses_as_conditional_flashback_static() {
        let result = parse(
            "Create a 3/3 green Phyrexian Beast creature token with toxic 1. (Players dealt combat damage by it also get a poison counter.)\nCorrupted — As long as an opponent has three or more poison counters and this card is in your graveyard, it has flashback {2}{G}. (You may cast this card from your graveyard for its flashback cost. Then exile it.)",
            "Viral Spawning",
            &[],
            &["Sorcery"],
            &[],
        );
        assert_eq!(result.statics.len(), 1);
        let static_def = &result.statics[0];
        assert_eq!(static_def.affected, Some(TargetFilter::SelfRef));
        assert!(
            static_def
                .modifications
                .contains(&ContinuousModification::AddKeyword {
                    keyword: Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
                        generic: 2,
                        shards: vec![crate::types::mana::ManaCostShard::Green],
                    })),
                }),
            "missing flashback keyword: {:?}",
            static_def.modifications
        );
        assert!(
            matches!(static_def.condition, Some(StaticCondition::And { .. })),
            "expected conjunctive static condition, got {:?}",
            static_def.condition
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
    fn each_opponent_with_no_cards_in_hand_preserves_condition() {
        let def = parse_effect_chain(
            "each opponent with no cards in hand loses 10 life",
            crate::types::ability::AbilityKind::Spell,
        );

        assert_eq!(def.player_scope, Some(PlayerFilter::Opponent));
        assert!(matches!(*def.effect, Effect::LoseLife { .. }));
        assert!(matches!(
            def.condition,
            Some(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller
                    }
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            })
        ));
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
    fn spell_cost_reduction_for_creatures_that_attacked_stays_static() {
        let r = parse(
            "This spell costs {1} less to cast for each creature that attacked this turn.\nDraw three cards.",
            "Rowdy Research",
            &[],
            &["Instant"],
            &[],
        );

        assert_eq!(r.abilities.len(), 1);
        assert!(
            matches!(*r.abilities[0].effect, Effect::Draw { .. }),
            "real spell effect should be preserved, got {:?}",
            r.abilities[0].effect
        );
        assert_eq!(r.statics.len(), 1);
        let StaticMode::ReduceCost {
            amount: ManaCost::Cost { generic: 1, .. },
            dynamic_count:
                Some(QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(filter),
                }),
            ..
        } = &r.statics[0].mode
        else {
            panic!(
                "expected self-spell ReduceCost over attacked creatures, got {:?}",
                r.statics[0].mode
            );
        };
        assert!(filter
            .type_filters
            .iter()
            .any(|filter| matches!(filter, TypeFilter::Creature)));
        assert!(filter
            .properties
            .iter()
            .any(|prop| matches!(prop, FilterProp::AttackedThisTurn)));
        assert!(matches!(r.statics[0].affected, Some(TargetFilter::SelfRef)));
        assert_eq!(r.statics[0].active_zones, vec![Zone::Hand, Zone::Stack]);
        assert!(
            r.parse_warnings
                .iter()
                .all(|warning| warning.split_whitespace().next() != Some("Swallow:DynamicQty")),
            "unexpected DynamicQty warning: {:?}",
            r.parse_warnings
        );
    }

    #[test]
    fn spell_cost_reduction_for_creatures_that_attacked_preserves_damage_effect() {
        let r = parse(
            "This spell costs {1} less to cast for each creature that attacked this turn.\nWitchstalker Frenzy deals 5 damage to target creature.",
            "Witchstalker Frenzy",
            &[],
            &["Instant"],
            &[],
        );

        assert_eq!(r.abilities.len(), 1);
        assert!(
            matches!(*r.abilities[0].effect, Effect::DealDamage { .. }),
            "real spell effect should be preserved, got {:?}",
            r.abilities[0].effect
        );
        assert_eq!(r.statics.len(), 1);
        assert!(
            matches!(r.statics[0].mode, StaticMode::ReduceCost { .. }),
            "cost-reduction sentence should be a static, got {:?}",
            r.statics[0].mode
        );
        assert!(
            r.abilities
                .iter()
                .all(|ability| !matches!(*ability.effect, Effect::CastFromZone { .. })),
            "cost-reduction sentence must not become CastFromZone: {:?}",
            r.abilities
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
            Some(crate::types::ability::Duration::UntilNextTurnOf {
                player: crate::types::ability::PlayerScope::Controller,
            })
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
    fn biomass_mutation_parses_as_generic_effect_with_dynamic_set_pt() {
        // CR 613.4b + CR 107.3m: "Creatures you control have base power and
        // toughness X/X until end of turn" is a one-shot layer-7b set effect.
        // The spell is an instant with {X} in cost, so X resolves to CostXPaid.
        use crate::types::ability::{ContinuousModification, Effect, QuantityExpr, QuantityRef};
        let r = parse(
            "Creatures you control have base power and toughness X/X until end of turn.",
            "Biomass Mutation",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1, "expected one spell ability");
        let eff = &*r.abilities[0].effect;
        let Effect::GenericEffect {
            static_abilities, ..
        } = eff
        else {
            panic!("expected GenericEffect, got {eff:?}");
        };
        assert_eq!(static_abilities.len(), 1);
        let mods = &static_abilities[0].modifications;
        let has_p = mods.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::SetPowerDynamic {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::CostXPaid
                    }
                }
            )
        });
        let has_t = mods.iter().any(|m| {
            matches!(
                m,
                ContinuousModification::SetToughnessDynamic {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::CostXPaid
                    }
                }
            )
        });
        assert!(has_p, "missing SetPowerDynamic(CostXPaid) in {mods:?}");
        assert!(has_t, "missing SetToughnessDynamic(CostXPaid) in {mods:?}");
    }

    #[test]
    fn karn_sydri_artifact_animation_has_dynamic_mana_value_pt_no_warning() {
        for (name, text) in [
            (
                "Karn, Silver Golem",
                "{1}: Target noncreature artifact becomes an artifact creature with power and toughness each equal to its mana value until end of turn.",
            ),
            (
                "Sydri, Galvanic Genius",
                "{U}: Target noncreature artifact becomes an artifact creature with power and toughness each equal to its mana value until end of turn.",
            ),
        ] {
            let r = parse(text, name, &[], &["Artifact"], &[]);
            assert!(
                r.parse_warnings
                    .iter()
                    .all(|warning| warning.split_whitespace().next() != Some("Swallow:DynamicQty")),
                "unexpected DynamicQty warning for {name}: {:?}",
                r.parse_warnings
            );
            assert_eq!(r.abilities.len(), 1, "{name}: expected one activated ability");

            let Effect::GenericEffect {
                target: Some(TargetFilter::Typed(tf)),
                static_abilities,
                duration: Some(crate::types::ability::Duration::UntilEndOfTurn),
            } = r.abilities[0].effect.as_ref()
            else {
                panic!("{name}: expected UEOT GenericEffect, got {:?}", r.abilities[0].effect);
            };
            assert!(tf.type_filters.contains(&TypeFilter::Artifact));
            assert!(
                tf.type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Creature)))
            );
            assert_eq!(static_abilities.len(), 1);

            let mods = &static_abilities[0].modifications;
            let expected = QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Recipient,
                },
            };
            assert!(mods.contains(&ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Artifact,
            }));
            assert!(mods.contains(&ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Creature,
            }));
            assert!(mods.contains(&ContinuousModification::SetPowerDynamic {
                value: expected.clone(),
            }));
            assert!(mods.contains(&ContinuousModification::SetToughnessDynamic {
                value: expected,
            }));
        }
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
                ..
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
                ..
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

    /// Satyr Wayfinder: "reveal the top four cards" → Dig with reveal=true,
    /// continuation patches keep_count, filter, rest_destination from "you may put a land card
    /// from among them into your hand. Put the rest into your graveyard."
    #[test]
    fn satyr_wayfinder_reveal_dig_from_among() {
        let result = parse_with_keyword_names(
            "When this creature enters, reveal the top four cards of your library. You may put a land card from among them into your hand. Put the rest into your graveyard.",
            "Satyr Wayfinder",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(result.triggers.len(), 1, "should have one ETB trigger");
        let execute = result.triggers[0]
            .execute
            .as_ref()
            .expect("trigger should have execute");
        match &*execute.effect {
            Effect::Dig {
                count,
                destination,
                keep_count,
                up_to,
                filter,
                rest_destination,
                reveal,
            } => {
                assert_eq!(
                    count,
                    &QuantityExpr::Fixed { value: 4 },
                    "dig count should be 4"
                );
                assert!(
                    reveal,
                    "should be reveal=true for 'reveal the top' (CR 701.20a)"
                );
                assert_eq!(destination, &Some(Zone::Hand), "kept cards go to hand");
                assert_eq!(keep_count, &Some(1), "keep up to 1 (a land card)");
                assert!(up_to, "'you may' = up to");
                assert!(
                    matches!(filter, TargetFilter::Typed(TypedFilter { ref type_filters, .. })
                        if type_filters.contains(&TypeFilter::Land)),
                    "filter should require lands, got {:?}",
                    filter,
                );
                assert_eq!(
                    rest_destination,
                    &Some(Zone::Graveyard),
                    "rest go to graveyard"
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

    #[test]
    fn analyze_the_pollen_parses_collect_evidence_search_override() {
        fn contains_reveal_top(ability: &AbilityDefinition) -> bool {
            matches!(&*ability.effect, Effect::RevealTop { .. })
                || ability
                    .sub_ability
                    .as_ref()
                    .is_some_and(|sub| contains_reveal_top(sub))
                || ability
                    .else_ability
                    .as_ref()
                    .is_some_and(|sub| contains_reveal_top(sub))
        }

        let result = parse_with_keyword_names(
            "As an additional cost to cast this spell, you may collect evidence 8. (Exile cards with total mana value 8 or greater from your graveyard.)\nSearch your library for a basic land card. If evidence was collected, instead search your library for a creature or land card. Reveal that card, put it into your hand, then shuffle.",
            "Analyze the Pollen",
            &["Collect evidence"],
            &["Sorcery"],
            &[],
        );

        assert_eq!(
            result.additional_cost,
            Some(AdditionalCost::Optional(AbilityCost::CollectEvidence {
                amount: 8,
            }))
        );
        assert_eq!(result.abilities.len(), 1);
        let ability = &result.abilities[0];
        match &*ability.effect {
            Effect::SearchLibrary {
                filter,
                count,
                reveal,
                ..
            } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                assert!(*reveal);
                match filter {
                    TargetFilter::Typed(tf) => {
                        assert!(tf.type_filters.contains(&TypeFilter::Land));
                        assert!(tf.properties.iter().any(|prop| matches!(
                            prop,
                            crate::types::ability::FilterProp::HasSupertype {
                                value: crate::types::card_type::Supertype::Basic
                            }
                        )));
                    }
                    other => panic!("Expected typed land filter, got {:?}", other),
                }
            }
            other => panic!("Expected SearchLibrary, got {:?}", other),
        }

        let override_search = ability
            .sub_ability
            .as_ref()
            .expect("expected override search");
        assert_eq!(
            override_search.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        );
        match &*override_search.effect {
            Effect::SearchLibrary {
                filter,
                count,
                reveal,
                ..
            } => {
                assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                assert!(*reveal);
                match filter {
                    TargetFilter::Or { filters } => {
                        assert_eq!(filters.len(), 2);
                        assert!(filters.iter().any(|filter| matches!(
                            filter,
                            TargetFilter::Typed(tf)
                                if tf.type_filters.contains(&TypeFilter::Creature)
                        )));
                        assert!(filters.iter().any(|filter| matches!(
                            filter,
                            TargetFilter::Typed(tf)
                                if tf.type_filters.contains(&TypeFilter::Land)
                        )));
                    }
                    other => panic!("Expected creature-or-land filter, got {:?}", other),
                }
            }
            other => panic!("Expected override SearchLibrary, got {:?}", other),
        }

        let to_hand = override_search
            .else_ability
            .as_ref()
            .expect("expected shared continuation");
        assert!(matches!(
            *to_hand.effect,
            Effect::ChangeZone {
                destination: Zone::Hand,
                ..
            }
        ));
        let shuffle = to_hand.sub_ability.as_ref().expect("expected shuffle");
        assert!(matches!(*shuffle.effect, Effect::Shuffle { .. }));
        assert!(!contains_reveal_top(ability));
    }

    // ── Time Travel (CR 701.56) ──

    #[test]
    fn time_travel_standalone_spell() {
        let r = parse(
            "Time travel.\nDraw a card.",
            "Wibbly-Wobbly, Timey-Wimey",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 2);
        assert!(matches!(*r.abilities[0].effect, Effect::TimeTravel));
        assert!(matches!(*r.abilities[1].effect, Effect::Draw { .. }));
    }

    #[test]
    fn time_travel_in_trigger() {
        let r = parse(
            "Whenever this creature deals combat damage to a player, time travel.",
            "Time Beetle",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        let exec = r.triggers[0].execute.as_ref().unwrap();
        assert!(matches!(*exec.effect, Effect::TimeTravel));
    }

    #[test]
    fn time_travel_activated_ability() {
        let r = parse(
            "{4}, {T}: Time travel. Activate only as a sorcery.",
            "Rotating Fireplace",
            &[],
            &["Artifact"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert!(matches!(*r.abilities[0].effect, Effect::TimeTravel));
        assert!(r.abilities[0].sorcery_speed);
    }

    // ── Exert (CR 701.43d) ──

    #[test]
    fn exert_with_when_you_do_pump() {
        let r = parse(
            "You may exert this creature as it attacks. When you do, it gets +1/+3 and gains lifelink until end of turn.",
            "Glory-Bound Initiate",
            &[],
            &["Creature"],
            &["Human", "Warrior"],
        );
        assert_eq!(r.triggers.len(), 1);
        assert_eq!(r.triggers[0].mode, TriggerMode::Exerted);
        let exec = r.triggers[0].execute.as_ref().unwrap();
        // The "gets +1/+3 and gains lifelink" is a continuous modification (GenericEffect),
        // not a direct Pump — parse_effect_chain handles this composite pattern.
        assert!(
            matches!(
                *exec.effect,
                Effect::GenericEffect { .. } | Effect::Pump { .. }
            ),
            "expected GenericEffect or Pump, got {:?}",
            exec.effect
        );
    }

    #[test]
    fn exert_standalone_line() {
        let r = parse(
            "You may exert this creature as it attacks.\nWhenever you exert a creature, you may discard a card. If you do, draw a card.",
            "Battlefield Scavenger",
            &[],
            &["Creature"],
            &[],
        );
        // Standalone exert line produces no output (trigger is separate)
        assert!(r.abilities.is_empty());
        assert_eq!(r.triggers.len(), 1);
        assert_eq!(
            r.triggers[0].mode,
            TriggerMode::Unknown("Whenever you exert a creature".to_string())
        );
    }

    #[test]
    fn exert_with_card_name() {
        let r = parse(
            "You may exert Anep as it attacks. When you do, exile the top two cards of your library. Until the end of your next turn, you may play those cards.",
            "Anep, Vizier of Hazoret",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        assert_eq!(r.triggers[0].mode, TriggerMode::Exerted);
    }

    #[test]
    fn exert_conditional() {
        let r = parse(
            "If this creature hasn't been exerted this turn, you may exert it as it attacks. When you do, untap all other creatures you control and after this phase, there is an additional combat phase.",
            "Combat Celebrant",
            &[],
            &["Creature"],
            &[],
        );
        assert_eq!(r.triggers.len(), 1);
        assert_eq!(r.triggers[0].mode, TriggerMode::Exerted);
    }

    // ── Leveler activated abilities (CR 711.2a + CR 711.2b) ──

    #[test]
    fn leveler_activated_abilities_get_level_counter_range() {
        let r = parse(
            "Level up {3}{R}\nLEVEL 1-2\n2/3\n{T}: This creature deals 1 damage to any target.\nLEVEL 3+\n2/4\n{T}: This creature deals 3 damage to any target.",
            "Brimstone Mage",
            &[Keyword::LevelUp(ManaCost::generic(0))],
            &["Creature"],
            &[],
        );
        // Two level-gated activated abilities
        let level_gated: Vec<_> = r
            .abilities
            .iter()
            .filter(|a| {
                a.activation_restrictions
                    .iter()
                    .any(|ar| matches!(ar, ActivationRestriction::LevelCounterRange { .. }))
            })
            .collect();
        assert_eq!(level_gated.len(), 2);

        // First level-gated ability: LEVEL 1-2
        assert_eq!(level_gated[0].kind, AbilityKind::Activated);
        assert!(level_gated[0].activation_restrictions.contains(
            &ActivationRestriction::LevelCounterRange {
                minimum: 1,
                maximum: Some(2),
            }
        ));

        // Second level-gated ability: LEVEL 3+
        assert_eq!(level_gated[1].kind, AbilityKind::Activated);
        assert!(level_gated[1].activation_restrictions.contains(
            &ActivationRestriction::LevelCounterRange {
                minimum: 3,
                maximum: None,
            }
        ));

        // No spurious triggers
        assert_eq!(r.triggers.len(), 0);
    }

    #[test]
    fn fatal_push_full_composition() {
        use crate::types::ability::AbilityCondition;

        // CR 608.2c: Two-line "instead" composition with ability word + MV conditions.
        // Base: Destroy target creature if MV ≤ 2
        // Revolt: Destroy that creature if MV ≤ 4 instead (when revolt active)
        let r = parse_oracle_text(
            "Destroy target creature if it has mana value 2 or less.\nRevolt \u{2014} Destroy that creature if it has mana value 4 or less instead if a permanent left the battlefield under your control this turn.",
            "Fatal Push",
            &[],
            &["Instant".to_string()],
            &[],
        );
        assert_eq!(
            r.abilities.len(),
            1,
            "should be ONE ability (instead composition)"
        );
        let ability = &r.abilities[0];

        // Base condition: TargetMatchesFilter with CmcLE(2)
        match &ability.condition {
            Some(AbilityCondition::TargetMatchesFilter { filter, .. }) => {
                if let TargetFilter::Typed(tf) = filter {
                    assert!(
                        tf.properties.iter().any(|p| matches!(
                            p,
                            FilterProp::Cmc {
                                comparator: Comparator::LE,
                                value: QuantityExpr::Fixed { value: 2 }
                            }
                        )),
                        "base should have CmcLE(2), got: {:?}",
                        tf.properties
                    );
                } else {
                    panic!("expected Typed filter on base condition");
                }
            }
            other => panic!("expected TargetMatchesFilter on base, got: {other:?}"),
        }

        // Sub-ability: ConditionInstead with And([Revolt, CmcLE(4)])
        let sub = ability
            .sub_ability
            .as_ref()
            .expect("should have sub_ability");
        match &sub.condition {
            Some(AbilityCondition::ConditionInstead { inner }) => match inner.as_ref() {
                AbilityCondition::And { conditions } => {
                    assert_eq!(conditions.len(), 2, "And should have 2 conditions");
                    // First: Revolt (QuantityCheck on zone-change count)
                    assert!(
                        matches!(&conditions[0], AbilityCondition::QuantityCheck { .. }),
                        "first condition should be QuantityCheck (revolt)"
                    );
                    // Second: CmcLE(4)
                    match &conditions[1] {
                        AbilityCondition::TargetMatchesFilter { filter, .. } => {
                            if let TargetFilter::Typed(tf) = filter {
                                assert!(
                                    tf.properties.iter().any(|p| matches!(
                                        p,
                                        FilterProp::Cmc {
                                            comparator: Comparator::LE,
                                            value: QuantityExpr::Fixed { value: 4 }
                                        }
                                    )),
                                    "revolt sub should have CmcLE(4), got: {:?}",
                                    tf.properties
                                );
                            } else {
                                panic!("expected Typed filter on revolt sub");
                            }
                        }
                        other => panic!("expected TargetMatchesFilter in And[1], got: {other:?}"),
                    }
                }
                other => panic!("expected And inside ConditionInstead, got: {other:?}"),
            },
            other => panic!("expected ConditionInstead on sub, got: {other:?}"),
        }
    }

    #[test]
    fn ferocious_ability_word_applies_power_condition_to_spell_effect() {
        use crate::types::ability::{AbilityCondition, QuantityRef};

        let r = parse_oracle_text(
            "You gain 5 life.\nFerocious \u{2014} You gain 10 life instead if you control a creature with power 4 or greater.",
            "Feed the Clan",
            &[],
            &["Instant".to_string()],
            &[],
        );
        assert!(r
            .parse_warnings
            .iter()
            .all(|warning| { warning.split_whitespace().next() != Some("Swallow:Condition_If") }));
        let base = r
            .abilities
            .first()
            .expect("expected base gain-life ability");
        let ferocious = base
            .sub_ability
            .as_ref()
            .expect("expected conditional ferocious branch");
        assert!(matches!(
            *ferocious.effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 10 },
                ..
            }
        ));
        let Some(AbilityCondition::ConditionInstead { inner }) = ferocious.condition.as_ref()
        else {
            panic!(
                "expected ferocious ConditionInstead, got {:?}",
                ferocious.condition
            );
        };
        let AbilityCondition::QuantityCheck {
            lhs,
            comparator,
            rhs,
        } = inner.as_ref()
        else {
            panic!("expected ferocious QuantityCheck, got {inner:?}");
        };
        assert_eq!(*comparator, Comparator::GE);
        assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
        let QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } = lhs
        else {
            panic!("expected ObjectCount lhs, got {lhs:?}");
        };
        let TargetFilter::Typed(filter) = filter else {
            panic!("expected typed creature filter");
        };
        assert_eq!(filter.controller, Some(ControllerRef::You));
        assert!(filter.properties.contains(&FilterProp::PowerGE {
            value: QuantityExpr::Fixed { value: 4 },
        }));
    }

    #[test]
    fn instead_if_condition_composes_without_ability_word_mapping() {
        use crate::types::ability::{AbilityCondition, QuantityRef};

        let r = parse_oracle_text(
            "Brimstone Volley deals 3 damage to any target.\nMorbid \u{2014} Brimstone Volley deals 5 damage instead if a creature died this turn.",
            "Brimstone Volley",
            &[],
            &["Instant".to_string()],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        let sub = r.abilities[0]
            .sub_ability
            .as_ref()
            .expect("instead branch should be attached to base ability");
        match &sub.condition {
            Some(AbilityCondition::ConditionInstead { inner }) => {
                assert!(matches!(
                    inner.as_ref(),
                    AbilityCondition::QuantityCheck {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::ZoneChangeCountThisTurn {
                                from: Some(Zone::Battlefield),
                                to: Some(Zone::Graveyard),
                                ..
                            },
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                    }
                ));
            }
            other => panic!("expected ConditionInstead quantity check, got {other:?}"),
        }
    }

    #[test]
    fn quantum_riddler_draw_line_parses_as_replacement_not_static() {
        let result = parse(
            "As long as you have one or fewer cards in hand, if you would draw one or more cards, you draw that many cards plus one instead.",
            "Quantum Riddler",
            &[],
            &["Creature"],
            &["Sphinx"],
        );

        assert_eq!(
            result.statics.len(),
            0,
            "line should not fall back to static parsing"
        );
        assert_eq!(
            result.replacements.len(),
            1,
            "line should parse as one replacement"
        );
        assert!(matches!(
            result.replacements[0].condition,
            Some(ReplacementCondition::OnlyIfQuantity { .. })
        ));
        assert_eq!(result.replacements[0].event, ReplacementEvent::Draw);
    }

    /// CR 205.3a: "[Subtype] [CoreType]" subject-predicate patterns like
    /// "Wizard creatures gain flying until end of turn" — the subtype+type compound
    /// must be fully consumed by parse_type_phrase so the subject-predicate parser
    /// can extract the filter.
    #[test]
    fn test_subtype_creatures_gain_keyword() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{ContinuousModification, Duration, TargetFilter, TypeFilter};
        use crate::types::keywords::Keyword;

        let def = parse_effect_chain(
            "wizard creatures gain flying until end of turn",
            crate::types::ability::AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::GenericEffect {
                static_abilities,
                duration,
                ..
            } => {
                assert_eq!(
                    *duration,
                    Some(Duration::UntilEndOfTurn),
                    "duration should be UntilEndOfTurn"
                );
                assert_eq!(static_abilities.len(), 1);
                let sa = &static_abilities[0];
                // Affected filter should include both Creature and Subtype("Wizard")
                if let Some(TargetFilter::Typed(tf)) = &sa.affected {
                    assert!(
                        tf.type_filters
                            .contains(&TypeFilter::Subtype("Wizard".to_string())),
                        "should contain Wizard subtype, got {:?}",
                        tf.type_filters
                    );
                    assert!(
                        tf.type_filters.contains(&TypeFilter::Creature),
                        "should contain Creature type, got {:?}",
                        tf.type_filters
                    );
                } else {
                    panic!("expected Typed filter, got {:?}", sa.affected);
                }
                assert!(sa.modifications.iter().any(|m| matches!(
                    m,
                    ContinuousModification::AddKeyword { keyword }
                        if *keyword == Keyword::Flying
                )));
            }
            other => panic!("expected GenericEffect, got {:?}", other),
        }
    }

    /// "Goblin creatures get +1/+1 until end of turn" — same [Subtype] [CoreType] pattern
    /// with a pump predicate instead of keyword grant.
    #[test]
    fn test_subtype_creatures_get_pump() {
        use crate::parser::oracle_effect::parse_effect_chain;

        let def = parse_effect_chain(
            "goblin creatures get +1/+1 until end of turn",
            crate::types::ability::AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::PumpAll { .. } => {}
            other => panic!("expected PumpAll, got {:?}", other),
        }
    }

    // CR 201.3 / CR 113.6: Petrified Hamlet — full four-line parse must
    // produce a ChangesZone trigger (choose a land card name, persist=true),
    // a continuous static granting `{T}: Add {C}.` to every land whose name
    // matches the chosen name, the CantBeActivated static on
    // `HasChosenName` sources, and the card's own `{T}: Add {C}.`
    // activated mana ability — zero Unimplemented ambiances.
    #[test]
    fn petrified_hamlet_full_parse() {
        use crate::types::ability::{ChoiceType, Effect};
        let text = "When this land enters, choose a land card name.\n\
                    Activated abilities of sources with the chosen name can't be activated unless they're mana abilities.\n\
                    Lands with the chosen name have \"{T}: Add {C}.\"\n\
                    {T}: Add {C}.";
        let r = parse(text, "Petrified Hamlet", &[], &["Land"], &[]);

        // No Unimplemented anywhere.
        for a in r.abilities.iter() {
            assert!(
                !matches!(*a.effect, Effect::Unimplemented { .. }),
                "ability Unimplemented: {:?}",
                a
            );
        }
        for t in &r.triggers {
            let exec = t.execute.as_ref().expect("trigger execute");
            assert!(
                !matches!(*exec.effect, Effect::Unimplemented { .. }),
                "trigger Unimplemented: {:?}",
                t
            );
        }

        // Trigger: choose-a-land-card-name with persist=true.
        assert_eq!(r.triggers.len(), 1);
        let trig = &r.triggers[0];
        assert_eq!(trig.mode, TriggerMode::ChangesZone);
        assert_eq!(trig.destination, Some(Zone::Battlefield));
        let trig_exec = trig.execute.as_ref().unwrap();
        assert!(
            matches!(
                *trig_exec.effect,
                Effect::Choose {
                    choice_type: ChoiceType::CardName,
                    persist: true,
                }
            ),
            "expected Choose{{CardName, persist:true}}, got {:?}",
            trig_exec.effect
        );

        // One activated mana ability ({T}: Add {C}).
        let mana_abils: Vec<_> = r
            .abilities
            .iter()
            .filter(|a| matches!(*a.effect, Effect::Mana { .. }))
            .collect();
        assert_eq!(mana_abils.len(), 1);

        // Two statics: CantBeActivated (HasChosenName) + continuous grant on
        // Lands-with-the-chosen-name.
        assert_eq!(r.statics.len(), 2);
        let has_cant_be_activated = r
            .statics
            .iter()
            .any(|s| matches!(&s.mode, StaticMode::CantBeActivated { .. }));
        assert!(has_cant_be_activated, "expected CantBeActivated static");

        let grant_static = r
            .statics
            .iter()
            .find(|s| matches!(&s.mode, StaticMode::Continuous))
            .expect("expected continuous grant static");
        match &grant_static.affected {
            Some(TargetFilter::And { filters }) => {
                assert_eq!(filters.len(), 2);
                assert_eq!(filters[1], TargetFilter::HasChosenName);
            }
            other => {
                panic!("expected And[Typed(Land), HasChosenName] for grant static, got {other:?}")
            }
        }
        assert_eq!(grant_static.modifications.len(), 1);
        assert!(matches!(
            &grant_static.modifications[0],
            ContinuousModification::GrantAbility { .. }
        ));
    }

    // CR 608.2 + CR 107.1a + CR 701.16a: Pox Plague — the "Each player loses
    // half their life, then discards half the cards in their hand, then
    // sacrifices half the permanents they control of their choice. Round down
    // each time." chain exercises all four fixes landed in the punisher-chain
    // commit:
    //   A. player_scope rewrite: `their life` / `their hand` → LifeTotal /
    //      HandSize so per-player iteration resolves against the rebound
    //      controller, not the empty targets list.
    //   B. half-rounded inner: `half the cards in their hand` parses through
    //      the new `parse_cards_in_possessive_zone` combinator, producing a
    //      HalfRounded count rather than collapsing to 1.
    //   C. Sacrifice.count: a dynamic count lifted from
    //      `half the permanents they control` into the new count field, and
    //      the embedded ObjectCount filter lifted into `Sacrifice.target` so
    //      eligibility matches the same set the count was computed against.
    //   D. trailing rounding: `Round down each time` consumed by
    //      `strip_trailing_rounding_annotation` and back-applied through
    //      `rewrite_rounding_mode` — the chunk does not become an
    //      Unimplemented effect.
    #[test]
    fn pox_plague_full_parse() {
        use crate::types::ability::{QuantityExpr, QuantityRef, RoundingMode};

        let r = parse(
            "Each player loses half their life, then discards half the cards in their hand, then sacrifices half the permanents they control of their choice. Round down each time.",
            "Pox Plague",
            &[],
            &["Sorcery"],
            &[],
        );

        // A single top-level ability with player_scope: All.
        assert_eq!(r.abilities.len(), 1);
        let ability = &r.abilities[0];
        assert!(
            matches!(
                ability.player_scope,
                Some(crate::types::ability::PlayerFilter::All)
            ),
            "expected player_scope All, got {:?}",
            ability.player_scope
        );

        // Fix A: LoseLife amount uses controller-scoped LifeTotal.
        match &*ability.effect {
            Effect::LoseLife { amount, .. } => match amount {
                QuantityExpr::HalfRounded { inner, rounding } => {
                    assert_eq!(*rounding, RoundingMode::Down);
                    assert!(
                        matches!(
                            **inner,
                            QuantityExpr::Ref {
                                qty: QuantityRef::LifeTotal {
                                    player: crate::types::ability::PlayerScope::Controller
                                }
                            }
                        ),
                        "expected LifeTotal, got {inner:?}"
                    );
                }
                other => panic!("expected HalfRounded LoseLife amount, got {other:?}"),
            },
            other => panic!("expected LoseLife top-level, got {other:?}"),
        }

        // Fix B + A: Discard count uses HalfRounded(HandSize).
        let discard = ability.sub_ability.as_ref().expect("discard sub_ability");
        match &*discard.effect {
            Effect::Discard { count, .. } => match count {
                QuantityExpr::HalfRounded { inner, rounding } => {
                    assert_eq!(*rounding, RoundingMode::Down);
                    assert!(
                        matches!(
                            **inner,
                            QuantityExpr::Ref {
                                qty: QuantityRef::HandSize {
                                    player: crate::types::ability::PlayerScope::Controller
                                }
                            }
                        ),
                        "expected HandSize, got {inner:?}"
                    );
                }
                other => panic!("expected HalfRounded Discard count, got {other:?}"),
            },
            other => panic!("expected Discard mid-chain, got {other:?}"),
        }

        // Fix C: Sacrifice carries HalfRounded(ObjectCount{Permanent,you-control})
        // as count, and the same Typed filter lifted into target.
        let sacrifice = discard.sub_ability.as_ref().expect("sacrifice sub_ability");
        match &*sacrifice.effect {
            Effect::Sacrifice { target, count } => {
                assert!(!count.is_up_to(), "expected non-UpTo sacrifice count");
                match count {
                    QuantityExpr::HalfRounded { inner, rounding } => {
                        assert_eq!(*rounding, RoundingMode::Down);
                        match &**inner {
                            QuantityExpr::Ref {
                                qty: QuantityRef::ObjectCount { filter },
                            } => match filter {
                                TargetFilter::Typed(tf) => {
                                    assert!(tf.type_filters.contains(&TypeFilter::Permanent));
                                }
                                other => panic!("expected Typed filter, got {other:?}"),
                            },
                            other => panic!("expected ObjectCount inner, got {other:?}"),
                        }
                    }
                    other => panic!("expected HalfRounded Sacrifice count, got {other:?}"),
                }
                match target {
                    TargetFilter::Typed(tf) => {
                        assert!(tf.type_filters.contains(&TypeFilter::Permanent));
                    }
                    other => panic!("expected Typed target lifted from count, got {other:?}"),
                }
            }
            other => panic!("expected Sacrifice tail, got {other:?}"),
        }

        // Fix D: "Round down each time" consumed — no Unimplemented anywhere.
        fn walk_no_unimpl(def: &crate::types::ability::AbilityDefinition) {
            assert!(
                !matches!(*def.effect, Effect::Unimplemented { .. }),
                "Unimplemented in Pox Plague chain: {:?}",
                def.effect
            );
            if let Some(sub) = def.sub_ability.as_ref() {
                walk_no_unimpl(sub);
            }
        }
        walk_no_unimpl(ability);
    }

    /// CR 702.94a + CR 400.3: End-to-end reproduction of Sliver Weftwinder's
    /// hand-grant line through the full `parse_oracle_text` pipeline.
    #[test]
    fn hand_grant_reaches_statics_through_full_pipeline() {
        let oracle = "Sliver cards in your hand have warp {3}.";
        let parsed = parse(oracle, "Sliver Weftwinder", &[], &["Creature"], &["Sliver"]);
        let hand_grant = parsed.statics.iter().find(|s| {
            s.mode == StaticMode::Continuous
                && s.affected
                    .as_ref()
                    .map(|a| a.extract_in_zone() == Some(Zone::Hand))
                    .unwrap_or(false)
        });
        assert!(
            hand_grant.is_some(),
            "hand-zone static should reach result.statics, got statics={:?}, abilities={:?}",
            parsed.statics,
            parsed.abilities,
        );
    }

    #[test]
    fn prevention_followup_if_this_way_does_not_emit_condition_warning() {
        let oracle = "Prevent the next X damage that would be dealt to target creature this turn, where X is your devotion to white. If damage is prevented this way, Acolyte's Reward deals that much damage to any target.";
        let parsed = parse(oracle, "Acolyte's Reward", &[], &["Instant"], &[]);

        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| warning.split_whitespace().next() != Some("Swallow:Condition_If")),
            "unexpected condition warning: {:?}",
            parsed.parse_warnings
        );

        let ability = parsed
            .abilities
            .first()
            .expect("expected prevention spell ability");
        assert!(matches!(*ability.effect, Effect::PreventDamage { .. }));
        assert!(
            ability.sub_ability.is_some(),
            "expected prevented-this-way follow-up sub-ability"
        );
    }

    #[test]
    fn may_cost_decline_if_you_dont_does_not_emit_condition_or_optional_warning() {
        let oracle = "({T}: Add {B} or {R}.)\nAs this land enters, you may pay 2 life. If you don't, it enters tapped.";
        let parsed = parse(
            oracle,
            "Blood Crypt",
            &[],
            &["Land"],
            &["Swamp", "Mountain"],
        );

        assert!(
            parsed.parse_warnings.iter().all(|warning| {
                let label = warning.split_whitespace().next();
                label != Some("Swallow:Condition_If") && label != Some("Swallow:Optional_YouMay")
            }),
            "unexpected replacement choice warning: {:?}",
            parsed.parse_warnings
        );
        assert_eq!(parsed.replacements.len(), 1);
    }

    #[test]
    fn granted_trigger_you_may_draw_does_not_emit_optional_warning() {
        let oracle = "Enchant creature\nEnchanted creature gets +1/+1 and has \"Whenever this creature deals combat damage to a player, you may draw a card.\"";
        let parsed = parse(
            oracle,
            "Curious Obsession",
            &[],
            &["Enchantment"],
            &["Aura"],
        );

        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| warning.split_whitespace().next() != Some("Swallow:Optional_YouMay")),
            "unexpected optional warning: {:?}",
            parsed.parse_warnings
        );
        assert!(
            parsed.statics.iter().any(|static_def| {
                static_def.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        ContinuousModification::GrantTrigger { trigger }
                            if trigger.optional
                    )
                })
            }),
            "expected optional granted trigger, got statics={:?}",
            parsed.statics
        );
    }

    #[test]
    fn emblem_trigger_you_may_draw_does_not_emit_optional_warning() {
        let oracle =
            "[-6]: You get an emblem with \"Whenever a land you control enters, you may draw a card.\"";
        let parsed = parse(
            oracle,
            "Nissa, Vital Force",
            &[],
            &["Planeswalker"],
            &["Nissa"],
        );

        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| warning.split_whitespace().next() != Some("Swallow:Optional_YouMay")),
            "unexpected optional warning: {:?}",
            parsed.parse_warnings
        );
        assert!(
            parsed.abilities.iter().any(|ability| {
                matches!(
                    &*ability.effect,
                    Effect::CreateEmblem { triggers, .. }
                        if triggers.iter().any(|trigger| trigger.optional)
                )
            }),
            "expected emblem with optional trigger, got abilities={:?}",
            parsed.abilities
        );
    }

    #[test]
    fn must_block_if_able_static_does_not_emit_condition_warning() {
        let oracle = "Defender\nThis creature blocks each combat if able.";
        let parsed = parse(
            oracle,
            "Razorgrass Screen",
            &[Keyword::Defender],
            &["Artifact", "Creature"],
            &["Wall"],
        );

        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| warning.split_whitespace().next() != Some("Swallow:Condition_If")),
            "unexpected condition warning: {:?}",
            parsed.parse_warnings
        );
        assert!(parsed
            .statics
            .iter()
            .any(|static_def| static_def.mode == StaticMode::MustBlock));
    }

    #[test]
    fn temporary_comma_grant_must_attack_if_able_does_not_emit_condition_warning() {
        let oracle = "Damage can't be prevented this turn.\nCreatures you control have double strike, trample, and must attack if able until end of turn.";
        let parsed = parse(oracle, "Math is for Blockers", &[], &["Sorcery"], &[]);

        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| warning.split_whitespace().next() != Some("Swallow:Condition_If")),
            "unexpected condition warning: {:?}",
            parsed.parse_warnings
        );
        assert!(parsed.abilities.iter().any(|ability| {
            matches!(
                &*ability.effect,
                Effect::GenericEffect { static_abilities, .. }
                    if static_abilities
                        .iter()
                        .any(|static_def| static_def.mode == StaticMode::MustAttack)
            )
        }));
    }

    #[test]
    fn city_blessing_activation_restriction_does_not_emit_condition_warning() {
        let oracle = "Ascend (If you control ten or more permanents, you get the city's blessing for the rest of the game.)\n{T}: Add {C}.\n{5}, {T}: Draw a card. Activate only if you have the city's blessing.";
        let parsed = parse(oracle, "Arch of Orazca", &[], &["Land"], &[]);

        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| warning.split_whitespace().next() != Some("Swallow:Condition_If")),
            "unexpected condition warning: {:?}",
            parsed.parse_warnings
        );
        let draw_ability = parsed
            .abilities
            .iter()
            .find(|ability| matches!(*ability.effect, Effect::Draw { .. }))
            .expect("expected draw ability");
        assert!(draw_ability
            .activation_restrictions
            .iter()
            .any(|restriction| matches!(
                restriction,
                ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::HasCityBlessing)
                }
            )));
    }

    #[test]
    fn normalized_source_power_activation_restriction_does_not_emit_condition_warning() {
        let oracle = "{T}: This creature deals 4 damage to target creature. Activate only if this creature's power is 4 or greater.";
        let parsed = parse(
            oracle,
            "Bloodshot Trainee",
            &[],
            &["Creature"],
            &["Goblin", "Warrior"],
        );

        assert_eq!(parsed.parse_warnings, Vec::<String>::new());
        let damage_ability = parsed
            .abilities
            .iter()
            .find(|ability| matches!(*ability.effect, Effect::DealDamage { .. }))
            .expect("expected damage ability");
        assert!(damage_ability
            .activation_restrictions
            .iter()
            .any(|restriction| matches!(
                restriction,
                ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::SourcePowerAtLeast { minimum: 4 })
                }
            )));
    }

    #[test]
    fn instant_or_sorcery_cast_activation_restriction_does_not_emit_condition_warning() {
        let oracle = "{T}: You gain 2 life. Activate only if you've cast an instant or sorcery spell this turn.";
        let parsed = parse(oracle, "Potioner's Trove", &[], &["Artifact"], &[]);

        assert_eq!(parsed.parse_warnings, Vec::<String>::new());
        let gain_life_ability = parsed
            .abilities
            .iter()
            .find(|ability| matches!(*ability.effect, Effect::GainLife { .. }))
            .expect("expected gain-life ability");
        assert!(gain_life_ability
            .activation_restrictions
            .iter()
            .any(|restriction| matches!(
                restriction,
                ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::YouCastSpellThisTurn {
                        filter: Some(TargetFilter::Or { filters })
                    })
                } if filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(TypedFilter { type_filters, .. })
                        if type_filters == &vec![TypeFilter::Instant]
                )) && filters.iter().any(|filter| matches!(
                    filter,
                    TargetFilter::Typed(TypedFilter { type_filters, .. })
                        if type_filters == &vec![TypeFilter::Sorcery]
                ))
            )));
    }

    #[test]
    fn dynamic_mana_per_color_does_not_emit_dynamic_qty_warning() {
        let oracle =
            "Vivid — {T}: For each color among permanents you control, add one mana of that color.";
        let parsed = parse(oracle, "Bloom Tender", &[], &["Creature"], &[]);

        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| warning.split_whitespace().next() != Some("Swallow:DynamicQty")),
            "unexpected dynamic quantity warning: {:?}",
            parsed.parse_warnings
        );

        let ability = parsed
            .abilities
            .first()
            .expect("expected parsed mana ability");
        assert!(matches!(
            &*ability.effect,
            Effect::Mana {
                produced: crate::types::ability::ManaProduction::DistinctColorsAmongPermanents { .. },
                ..
            }
        ));
    }

    #[test]
    fn alrund_static_sum_for_each_does_not_emit_dynamic_qty_warning() {
        let oracle = "Alrund gets +1/+1 for each card in your hand and each foretold card you own in exile.\n\
             At the beginning of your end step, choose a card type, then reveal the top two cards of your library. \
             Put all cards of the chosen type revealed this way into your hand and the rest on the bottom of your library in any order.";
        let parsed = parse(
            oracle,
            "Alrund, God of the Cosmos",
            &[],
            &["Creature"],
            &["God"],
        );

        assert_eq!(
            parsed.triggers.len(),
            1,
            "end-step trigger must remain parsed"
        );
        assert_eq!(
            parsed.triggers[0].phase,
            Some(crate::types::phase::Phase::End)
        );
        assert_eq!(parsed.statics.len(), 1, "expected Alrund static pump");
        let static_def = &parsed.statics[0];
        assert!(
            static_def
                .modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicPower { value } if matches!(value, QuantityExpr::Sum { exprs } if exprs.len() == 2))),
            "expected dynamic power Sum, got {:?}",
            static_def.modifications
        );
        assert!(
            static_def.modifications.iter().all(|m| !matches!(
                m,
                ContinuousModification::AddPower { .. }
                    | ContinuousModification::AddToughness { .. }
            )),
            "must not emit fixed P/T mods: {:?}",
            static_def.modifications
        );
        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| warning.split_whitespace().next() != Some("Swallow:DynamicQty")),
            "unexpected DynamicQty warning: {:?}",
            parsed.parse_warnings
        );
    }

    #[test]
    fn coat_of_arms_velis_vel_static_shared_type_no_dynamic_qty_warning() {
        for (name, types, subtypes, oracle) in [
            (
                "Coat of Arms",
                &["Artifact"][..],
                &[][..],
                "Each creature gets +1/+1 for each other creature on the battlefield that shares at least one creature type with it. (For example, if two Goblin Warriors and a Goblin Shaman are on the battlefield, each gets +2/+2.)",
            ),
            (
                "Velis Vel",
                &["Plane"][..],
                &[][..],
                "Each creature gets +1/+1 for each other creature on the battlefield that shares at least one creature type with it. (For example, if two Elemental Shamans and an Elemental Spirit are on the battlefield, each gets +2/+2.)\nWhenever chaos ensues, target creature gains all creature types until end of turn.",
            ),
        ] {
            let parsed = parse(oracle, name, &[], types, subtypes);
            assert!(
                parsed
                    .parse_warnings
                    .iter()
                    .all(|warning| warning.split_whitespace().next() != Some("Swallow:DynamicQty")),
                "unexpected DynamicQty warning for {name}: {:?}",
                parsed.parse_warnings
            );

            let mut matching_static = None;
            for static_def in &parsed.statics {
                if static_def.affected == Some(TargetFilter::Typed(TypedFilter::creature())) {
                    matching_static = Some(static_def);
                    break;
                }
            }
            let static_def = matching_static.expect("expected global creature static");
            let expected = QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Creature],
                        controller: None,
                        properties: vec![
                            FilterProp::Another,
                            FilterProp::SharesQuality {
                                quality: SharedQuality::CreatureType,
                                reference: Some(Box::new(TargetFilter::ParentTarget)),
                                relation: SharedQualityRelation::Shares,
                            },
                        ],
                    }),
                },
            };

            assert!(
                static_def.modifications.iter().any(|m| matches!(
                    m,
                    ContinuousModification::AddDynamicPower { value } if value == &expected
                )),
                "expected dynamic power for {name}, got {:?}",
                static_def.modifications
            );
            assert!(
                static_def.modifications.iter().any(|m| matches!(
                    m,
                    ContinuousModification::AddDynamicToughness { value } if value == &expected
                )),
                "expected dynamic toughness for {name}, got {:?}",
                static_def.modifications
            );
            assert!(
                static_def.modifications.iter().all(|m| !matches!(
                    m,
                    ContinuousModification::AddPower { .. }
                        | ContinuousModification::AddToughness { .. }
                )),
                "must not emit fixed P/T mods for {name}: {:?}",
                static_def.modifications
            );
        }
    }

    #[test]
    fn gauntlets_treefolk_umbra_assign_damage_from_toughness_no_dynamic_qty_warning() {
        for (name, oracle) in [
            (
                "Gauntlets of Light",
                "Enchant creature\nEnchanted creature gets +0/+2 and assigns combat damage equal to its toughness rather than its power.\nEnchanted creature has \"{2}{W}: Untap this creature.\"",
            ),
            (
                "Treefolk Umbra",
                "Enchant creature\nEnchanted creature gets +0/+2 and assigns combat damage equal to its toughness rather than its power.\nUmbra armor",
            ),
        ] {
            let parsed = parse(oracle, name, &[], &["Enchantment"], &["Aura"]);
            assert!(
                parsed
                    .parse_warnings
                    .iter()
                    .all(|warning| !matches!(
                        warning.split_whitespace().next(),
                        Some("Swallow:DynamicQty" | "Swallow:Condition_AsLongAs")
                    )),
                "unexpected toughness-damage warning for {name}: {:?}",
                parsed.parse_warnings
            );

            let static_def = parsed
                .statics
                .iter()
                .find(|static_def| {
                    static_def.affected
                        == Some(TargetFilter::Typed(
                            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
                        ))
                        && static_def
                            .modifications
                            .contains(&ContinuousModification::AddToughness { value: 2 })
                })
                .expect("expected enchanted creature +0/+2 static");
            assert!(static_def
                .modifications
                .contains(&ContinuousModification::AssignDamageFromToughness));
        }
    }

    #[test]
    fn attached_conditional_toughness_damage_cards_no_dynamic_qty_warning() {
        for (name, types, subtypes, expected_props, oracle) in [
            (
                "Bark of Doran",
                &["Artifact"][..],
                &["Equipment"][..],
                vec![FilterProp::EquippedBy, FilterProp::ToughnessGTPower],
                "Equipped creature gets +0/+1.\nAs long as equipped creature's toughness is greater than its power, it assigns combat damage equal to its toughness rather than its power.\nEquip {1}",
            ),
            (
                "Solid Footing",
                &["Enchantment"][..],
                &["Aura"][..],
                vec![
                    FilterProp::EnchantedBy,
                    FilterProp::WithKeyword {
                        value: Keyword::Vigilance,
                    },
                ],
                "Flash\nEnchant creature\nEnchanted creature gets +1/+1.\nAs long as enchanted creature has vigilance, it assigns combat damage equal to its toughness rather than its power.",
            ),
        ] {
            let parsed = parse(oracle, name, &[], types, subtypes);
            assert!(
                parsed
                    .parse_warnings
                    .iter()
                    .all(|warning| !matches!(
                        warning.split_whitespace().next(),
                        Some("Swallow:DynamicQty" | "Swallow:Condition_AsLongAs")
                    )),
                "unexpected toughness-damage warning for {name}: {:?}",
                parsed.parse_warnings
            );

            let static_def = parsed
                .statics
                .iter()
                .find(|static_def| {
                    static_def.affected
                        == Some(TargetFilter::Typed(
                            TypedFilter::creature().properties(expected_props.clone()),
                        ))
                })
                .expect("expected attached conditional toughness-damage static");
            assert!(static_def
                .modifications
                .contains(&ContinuousModification::AssignDamageFromToughness));
        }
    }

    // ------------------------------------------------------------------
    // merge_ability_condition — single-authority merge for ability-word
    // plus literal-if condition composition.
    // ------------------------------------------------------------------

    fn cond_delirium() -> AbilityCondition {
        AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::DistinctCardTypes {
                    source: crate::types::ability::CardTypeSetSource::Zone {
                        zone: crate::types::ability::ZoneRef::Graveyard,
                        scope: crate::types::ability::CountScope::Controller,
                    },
                },
            },
            comparator: crate::types::ability::Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 4 },
        }
    }

    fn cond_your_turn() -> AbilityCondition {
        AbilityCondition::IsYourTurn
    }

    fn cond_max_speed() -> AbilityCondition {
        AbilityCondition::HasMaxSpeed
    }

    #[test]
    fn merge_ability_condition_dedups_structural_equal() {
        // Delirium ability-word + literal "if there are four or more card types..."
        // both emit the same `QuantityCheck` — the merge should collapse to a single
        // leaf condition, not `And(X, X)`.
        let merged = merge_ability_condition(Some(cond_delirium()), cond_delirium());
        assert_eq!(merged, cond_delirium());
    }

    #[test]
    fn merge_ability_condition_wraps_distinct_in_and() {
        let merged = merge_ability_condition(Some(cond_your_turn()), cond_delirium());
        match merged {
            AbilityCondition::And { conditions } => {
                assert_eq!(conditions.len(), 2);
                assert_eq!(conditions[0], cond_your_turn());
                assert_eq!(conditions[1], cond_delirium());
            }
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn merge_ability_condition_flattens_nested_and() {
        // Existing is already `And`: appending a third distinct condition must not
        // produce `And(And(X, Y), Z)` — the result stays flat.
        let existing = AbilityCondition::And {
            conditions: vec![cond_your_turn(), cond_delirium()],
        };
        let merged = merge_ability_condition(Some(existing), cond_max_speed());
        match merged {
            AbilityCondition::And { conditions } => {
                assert_eq!(conditions.len(), 3);
                assert_eq!(conditions[0], cond_your_turn());
                assert_eq!(conditions[1], cond_delirium());
                assert_eq!(conditions[2], cond_max_speed());
            }
            other => panic!("expected flat And(3), got {other:?}"),
        }
    }

    #[test]
    fn merge_ability_condition_dedups_against_and_children() {
        // Appending a condition that already exists in an `And` is a no-op (no duplicate).
        let existing = AbilityCondition::And {
            conditions: vec![cond_your_turn(), cond_delirium()],
        };
        let merged = merge_ability_condition(Some(existing.clone()), cond_delirium());
        assert_eq!(merged, existing);
    }

    #[test]
    fn merge_ability_condition_none_returns_incoming() {
        let merged = merge_ability_condition(None, cond_delirium());
        assert_eq!(merged, cond_delirium());
    }

    /// End-to-end: parse actual Violent Urge Oracle text and assert the 2nd ability's
    /// condition is a single `QuantityCheck`, not `And(X, X)`. Guards against the
    /// ability-word/literal-if duplication bug at the dispatch layer.
    #[test]
    fn delirium_spell_condition_is_single_leaf_not_and() {
        let parsed = parse(
            "Target creature gets +1/+0 and gains first strike until end of turn.\n\
             Delirium — If there are four or more card types among cards in your graveyard, \
             that creature gains double strike until end of turn.",
            "Violent Urge",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(parsed.abilities.len(), 2, "expected two spell abilities");
        let second = &parsed.abilities[1];
        match &second.condition {
            Some(AbilityCondition::QuantityCheck { .. }) => {}
            Some(AbilityCondition::And { conditions }) => {
                panic!(
                    "delirium condition must not be wrapped in And, got And with \
                     {} children: {conditions:?}",
                    conditions.len()
                );
            }
            other => panic!("expected QuantityCheck, got {other:?}"),
        }
    }

    /// Regression: pin Helm of the Host's already-shipped non-legendary token
    /// behavior so a future refactor of `parse_except_clause` /
    /// `become_copy_except` cannot silently drop the `RemoveSupertype`
    /// modification.
    ///
    /// CR 707.9b: "Some copy effects modify a characteristic as part of the
    /// copying process. The final set of values for that characteristic
    /// becomes part of the copiable values of the copy." — "except the token
    /// isn't legendary" is exactly such a modification, lowered to
    /// `ContinuousModification::RemoveSupertype { Legendary }` and stamped
    /// onto the synthesized token at creation time so the legend rule
    /// (CR 704.5j) cannot collapse the token even when its source is a
    /// legendary creature.
    ///
    /// This test pins the parser side only — the resolver side is pinned by
    /// `copy_token_remove_supertype_strips_legendary_from_token` in
    /// `crates/engine/src/game/effects/token_copy.rs`.
    #[test]
    fn helm_of_the_host_emits_remove_supertype_legendary() {
        use crate::types::card_type::Supertype;

        let r = parse(
            "At the beginning of combat on your turn, create a token that's a \
             copy of equipped creature, except the token isn't legendary. That \
             token gains haste.\nEquip {5}",
            "Helm of the Host",
            &[Keyword::Equip(Default::default())],
            &["Artifact"],
            &["Equipment"],
        );

        // One trigger (the begin-combat copy-token trigger) and one activated
        // ability (Equip {5}).
        assert_eq!(
            r.triggers.len(),
            1,
            "expected exactly one trigger, got {}: {:?}",
            r.triggers.len(),
            r.triggers
                .iter()
                .map(|t| t.description.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
        );

        let trig = &r.triggers[0];
        let exec = trig
            .execute
            .as_ref()
            .expect("begin-combat trigger must have an execute body");

        // CR 707.9b + CR 205.4: top-level effect is `CopyTokenOf` with the
        // `RemoveSupertype { Legendary }` modification baked in. The token
        // copies "equipped creature" — the target filter is internal detail
        // tested elsewhere; this regression test pins ONLY the
        // additional_modifications, which is the load-bearing field for the
        // non-legendary semantic.
        match &*exec.effect {
            Effect::CopyTokenOf {
                additional_modifications,
                ..
            } => {
                assert!(
                    additional_modifications.contains(&ContinuousModification::RemoveSupertype {
                        supertype: Supertype::Legendary,
                    }),
                    "Helm of the Host must emit RemoveSupertype(Legendary); \
                     additional_modifications was {additional_modifications:?}"
                );
            }
            other => panic!("expected CopyTokenOf at trigger.execute.effect, got {other:?}"),
        }
    }

    /// Regression: pin Puresteel Paladin's Metalcraft static-grant-of-equip line
    /// so a future refactor of `try_parse_equip` / Priority 3 dispatch cannot
    /// resurface the `cost: Unimplemented("ment you control...")` misparse.
    ///
    /// CR 207.2c (Metalcraft ability word) + CR 113.3 (granted ability) +
    /// CR 613.1 (continuous effect): "Equipment you control have equip {0}"
    /// must parse as a static (`AddKeyword(Equip {0})` continuous modification),
    /// not as a malformed activated ability whose cost text begins mid-word
    /// inside "Equipment". The defect was a missing word-boundary guard in
    /// `try_parse_equip`: the keyword "equip" must terminate at a recognized
    /// boundary char, not slice off the first 5 bytes of "Equipment".
    #[test]
    fn puresteel_paladin_metalcraft_grant_parses_as_static_not_activated() {
        let r = parse(
            "Whenever an Equipment you control enters, you may draw a card.\n\
             Metalcraft — Equipment you control have equip {0} as long as you \
             control three or more artifacts.",
            "Puresteel Paladin",
            &[],
            &["Creature"],
            &["Human", "Knight"],
        );
        // No malformed activated ability — the granted-equip line is a static.
        assert!(
            r.abilities.is_empty(),
            "expected zero activated abilities (the granted-equip line is a \
             static, not an activation on Puresteel itself); got: {:?}",
            r.abilities
                .iter()
                .map(|a| a.description.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
        );
        // Exactly one static — the AddKeyword(Equip{0}) Metalcraft grant.
        assert_eq!(
            r.statics.len(),
            1,
            "expected one static (Metalcraft grant); got {}: {:?}",
            r.statics.len(),
            r.statics
                .iter()
                .map(|s| s.description.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
        );
        let s = &r.statics[0];
        assert!(
            s.condition.is_some(),
            "Metalcraft grant must carry the ability-word condition"
        );
    }

    /// Regression: defensive coverage for `try_parse_equip`'s word-boundary
    /// guard. "Equipment ..." (a sentence opening with the noun, no keyword
    /// "equip") and "Equipped ..." (the static-grant subject) must both
    /// fall through Priority 3 without producing an Activated/Attach ability.
    #[test]
    fn try_parse_equip_word_boundary_rejects_equipment_and_equipped() {
        // "equip" → matches (cost follows)
        assert!(super::try_parse_equip("Equip {2}").is_some());
        assert!(super::try_parse_equip("Equip — {3}").is_some());
        // "equipment" → must NOT match (different word)
        assert!(super::try_parse_equip("Equipment you control have equip {0}.").is_none());
        // "equipped" → caller's separate guard handles this, but defending
        // try_parse_equip itself is fail-safe.
        assert!(super::try_parse_equip("Equipped creature gets +2/+0.").is_none());
    }

    #[test]
    fn restricted_equip_costs_use_embedded_mana_cost() {
        for (line, expected_generic) in [
            ("Equip Elf {2}", 2),
            ("Equip creature token {1}", 1),
            ("Equip legendary creature {3}", 3),
            ("Equip commander {3}", 3),
            ("Equip {2} or {B}", 2),
        ] {
            let ability = super::try_parse_equip(line).expect("restricted equip should parse");
            assert!(
                matches!(
                    ability.cost,
                    Some(AbilityCost::Mana {
                        cost: ManaCost::Cost { generic, .. },
                    }) if generic == expected_generic
                ),
                "{line} parsed unexpected cost: {:?}",
                ability.cost
            );
        }
    }

    #[test]
    fn equip_once_per_turn_constraint_strips_from_cost() {
        let ability = super::try_parse_equip("Equip {0}. Activate only once each turn.")
            .expect("equip should parse");
        assert_eq!(
            ability.cost,
            Some(AbilityCost::Mana {
                cost: ManaCost::zero(),
            })
        );
        assert!(
            ability
                .activation_restrictions
                .contains(&ActivationRestriction::OnlyOnceEachTurn),
            "expected only-once-each-turn restriction: {:?}",
            ability.activation_restrictions
        );
    }

    #[test]
    fn plate_armor_equip_cost_reduction_stays_on_equip_ability() {
        let result = parse(
            "Equipped creature gets +3/+3 and has ward {1}.\n\
             Equip {3}. This ability costs {1} less to activate for each other Equipment you control.",
            "Plate Armor",
            &[Keyword::Equip(ManaCost::Cost {
                generic: 3,
                shards: vec![],
            })],
            &["Artifact"],
            &["Equipment"],
        );

        assert_eq!(result.abilities.len(), 1);
        let equip = &result.abilities[0];
        assert_eq!(
            equip.cost,
            Some(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 3,
                    shards: vec![],
                },
            })
        );
        let reduction = equip
            .cost_reduction
            .as_ref()
            .expect("equip ability should carry cost reduction");
        assert_eq!(reduction.amount_per, 1);
        match &reduction.count {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            } => match filter {
                TargetFilter::Typed(tf) => {
                    assert_eq!(
                        tf.controller,
                        Some(crate::types::ability::ControllerRef::You)
                    );
                    assert!(
                        tf.type_filters.iter().any(
                            |filter| matches!(filter, TypeFilter::Subtype(name) if name == "Equipment")
                        ),
                        "expected Equipment subtype, got {:?}",
                        tf.type_filters
                    );
                    assert!(
                        tf.properties
                            .iter()
                            .any(|property| matches!(property, FilterProp::Another)),
                        "expected Another property, got {:?}",
                        tf.properties
                    );
                }
                other => panic!("expected typed ObjectCount filter, got {:?}", other),
            },
            other => panic!("expected ObjectCount cost reduction, got {:?}", other),
        }

        assert_eq!(result.statics.len(), 1);
        let static_def = &result.statics[0];
        assert!(
            static_def.modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::AddPower { value: 3 }
            )),
            "missing +3 power modification: {:?}",
            static_def.modifications
        );
        assert!(
            static_def.modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::AddToughness { value: 3 }
            )),
            "missing +3 toughness modification: {:?}",
            static_def.modifications
        );
        assert!(
            static_def.modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Ward(WardCost::Mana(ManaCost::Cost {
                        generic: 1,
                        shards,
                    })),
                } if shards.is_empty()
            )),
            "missing ward {{1}} modification: {:?}",
            static_def.modifications
        );
        assert!(
            result
                .parse_warnings
                .iter()
                .all(|warning| warning.split_whitespace().next() != Some("Swallow:DynamicQty")),
            "unexpected DynamicQty warning: {:?}",
            result.parse_warnings
        );
    }

    /// Regression: pin the broader "Equipment you control have equip {N}"
    /// class — Astor (no ability-word prefix, no em-dash on the line) and
    /// Syr Gwyn (Knight-restricted equip {0}) were silently affected by the
    /// same `try_parse_equip` boundary defect. Both must parse cleanly as
    /// statics without producing a malformed activated ability on the source.
    /// CR 113.3 + CR 613.1.
    #[test]
    fn equipment_have_equip_grant_class_parses_as_static() {
        // Astor — bare "Equipment you control have equip {1}." with no
        // ability-word prefix. lower_starts_with("equip") fires here too
        // because "equipment" begins with the same five letters.
        let r = parse(
            "Equipment you control have equip {1}.\nVehicles you control have crew 1.",
            "Astor, Bearer of Blades",
            &[],
            &["Creature"],
            &["Human", "Warrior"],
        );
        assert!(
            r.abilities.is_empty(),
            "Astor: no malformed activated ability expected; got {:?}",
            r.abilities
                .iter()
                .map(|a| a.description.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            r.statics.len(),
            2,
            "Astor: expected two statics (equip + crew grants); got {}",
            r.statics.len()
        );

        // Syr Gwyn — "Equipment you control have equip Knight {0}." (Knight
        // sub-restriction on the granted equip ability).
        let r = parse(
            "Equipment you control have equip Knight {0}.",
            "Syr Gwyn, Hero of Ashvale",
            &[],
            &["Creature"],
            &["Human", "Knight"],
        );
        assert!(
            r.abilities.is_empty(),
            "Syr Gwyn: no malformed activated ability expected; got {:?}",
            r.abilities
                .iter()
                .map(|a| a.description.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            r.statics.len(),
            1,
            "Syr Gwyn: expected one static (equip Knight grant); got {}",
            r.statics.len()
        );
    }

    #[test]
    fn defiler_single_line_cost_reduction_parses_as_dedicated_static() {
        let r = parse(
            "Flying\nAs an additional cost to cast blue permanent spells, you may pay 2 life. Those spells cost {U} less to cast if you paid life this way. This effect reduces only the amount of blue mana you pay.\nWhenever you cast a blue permanent spell, draw a card.",
            "Defiler of Dreams",
            &[Keyword::Flying],
            &["Creature"],
            &["Phyrexian", "Sphinx"],
        );

        assert_eq!(r.statics.len(), 1, "expected Defiler static: {r:#?}");
        match &r.statics[0].mode {
            StaticMode::DefilerCostReduction {
                color,
                life_cost,
                mana_reduction,
            } => {
                assert_eq!(*color, ManaColor::Blue);
                assert_eq!(*life_cost, 2);
                assert_eq!(
                    mana_reduction,
                    &ManaCost::Cost {
                        shards: vec![ManaCostShard::Blue],
                        generic: 0,
                    }
                );
            }
            other => panic!("expected DefilerCostReduction, got {other:?}"),
        }
        assert!(
            r.parse_warnings.iter().all(|warning| {
                let tag = warning.split_whitespace().next();
                tag != Some("Swallow:Optional_YouMay") && tag != Some("Swallow:Condition_If")
            }),
            "unexpected Defiler warnings: {:?}",
            r.parse_warnings
        );
    }
}
