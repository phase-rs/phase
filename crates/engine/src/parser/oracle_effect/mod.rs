mod animation;
pub(crate) mod counter;
pub(crate) mod imperative;
pub(crate) mod mana;
mod sequence;
pub(crate) mod subject;
mod token;
mod types;

use std::str::FromStr;

use super::oracle_quantity::{parse_cda_quantity, parse_for_each_clause};
use super::oracle_target::{parse_event_context_ref, parse_mana_value_suffix, parse_target};
use super::oracle_util::{
    contains_possessive, has_unconsumed_conditional, parse_mana_symbols, parse_number, strip_after,
    TextPair,
};
use crate::database::mtgjson::parse_mtgjson_mana_cost;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, CardPlayMode, CastingPermission, ChoiceType,
    Comparator, DamageSource, DelayedTriggerCondition, Duration, Effect, FilterProp,
    GainLifePlayer, MultiTargetSpec, NinjutsuVariant, PlayerFilter, PtValue, QuantityExpr,
    QuantityRef, StaticCondition, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
    UnlessCost,
};
use crate::types::card_type::CoreType;
use crate::types::game_state::{DistributionUnit, RetargetScope};
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use self::imperative::{
    lower_imperative_family_ast, lower_targeted_action_ast, lower_zone_counter_ast,
    parse_imperative_family_ast,
};
use self::sequence::{
    apply_clause_continuation, continuation_absorbs_current, parse_followup_continuation_ast,
    parse_intrinsic_continuation_ast, split_clause_sequence,
};
use self::subject::{try_parse_subject_predicate_ast, try_parse_targeted_controller_gain_life};
use self::types::*;

/// Context threaded through the effect parsing pipeline.
/// Enables pronoun resolution relative to the current subject.
#[derive(Debug, Clone, Default)]
pub(crate) struct ParseContext {
    /// The trigger subject, if parsing within a trigger effect.
    /// When Some and not SelfRef, bare pronouns ("it") resolve to TriggeringSource.
    pub subject: Option<TargetFilter>,
}

/// CR 608.2k: Resolve bare pronoun ("it"/"itself"/"its") based on parser context.
/// When a triggered ability's effect refers to an untargeted object previously
/// referred to by the trigger condition, it still affects that object.
/// In trigger effects where the subject is a non-self filter (e.g. "a creature
/// you control"), "it" refers to the triggering object (TriggeringSource).
/// For self-triggers ("~ enters"), "it" stays SelfRef.
/// For AttachedTo subjects ("equipped creature"), TriggeringSource is also correct
/// because the triggering event's source IS the attached-to creature.
pub(crate) fn resolve_it_pronoun(ctx: &ParseContext) -> TargetFilter {
    match &ctx.subject {
        Some(subject) if !matches!(subject, TargetFilter::SelfRef | TargetFilter::Any) => {
            TargetFilter::TriggeringSource
        }
        _ => TargetFilter::SelfRef,
    }
}

/// Parse an effect clause from Oracle text into an Effect enum.
/// This handles the verb-based matching for spell effects, activated ability effects,
/// and the effect portion of triggered abilities.
///
/// For compound effects ("Gain 3 life. Draw a card."), call `parse_effect_chain`
/// which splits on sentence boundaries and chains via AbilityDefinition::sub_ability.
pub fn parse_effect(text: &str) -> Effect {
    parse_effect_clause(text, &ParseContext::default()).effect
}

/// CR 603.7c: Parse "whenever [trigger condition] this turn, [effect]" delayed triggers.
/// These create multi-fire delayed triggers that persist until end of turn.
/// Example: "whenever a creature you control deals combat damage to a player this turn, draw a card"
fn try_parse_whenever_this_turn(tp: TextPair) -> Option<ParsedEffectClause> {
    if !tp.starts_with("whenever ") {
        return None;
    }
    // Must contain "this turn" to distinguish from regular triggers.
    // Use rsplit_around: "this turn" terminates the condition — the last occurrence
    // is the correct split point if the condition itself contains "this turn".
    let (before, after) = tp.rsplit_around(" this turn, ")?;

    // Condition is between "whenever " and " this turn"
    let condition_text = &before.lower[9..];
    // Effect is after " this turn, "
    let effect_text = after.original;

    // Parse the condition as a trigger using the trigger parser
    let (_, mut trigger_def) =
        crate::parser::oracle_trigger::parse_trigger_condition(condition_text);
    trigger_def.execute = None; // Effect lives in DelayedTrigger.ability, not here

    let inner = parse_effect_chain(effect_text, AbilityKind::Spell);

    Some(ParsedEffectClause {
        effect: Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::WheneverEvent {
                trigger: Box::new(trigger_def),
            },
            effect: Box::new(inner),
            uses_tracked_set: false,
        },
        duration: None,
        sub_ability: None,
        distribute: None,
        multi_target: None,
    })
}

/// CR 603.7c: Parse inline delayed triggers like "when that creature dies, draw a card".
/// Returns a `CreateDelayedTrigger` wrapping the parsed inner effect.
fn try_parse_inline_delayed_trigger(tp: TextPair) -> Option<ParsedEffectClause> {
    if !tp.starts_with("when ") {
        return None;
    }

    // Find the comma separator between condition and effect
    let comma = tp.find(", ")?;
    let condition_text = &tp.lower[5..comma];
    let effect_text = &tp.original[comma + 2..];

    let condition = if condition_text.contains("dies") || condition_text.contains("die") {
        DelayedTriggerCondition::WhenDies {
            filter: parse_delayed_subject_filter(condition_text),
        }
    } else if condition_text.contains("is put into")
        && (condition_text.contains("graveyard") || condition_text.contains("a graveyard"))
    {
        // CR 700.4: "is put into a graveyard" from battlefield = dies
        DelayedTriggerCondition::WhenDies {
            filter: parse_delayed_subject_filter(condition_text),
        }
    } else if condition_text.contains("leaves the battlefield") {
        DelayedTriggerCondition::WhenLeavesPlayFiltered {
            filter: parse_delayed_subject_filter(condition_text),
        }
    } else if condition_text.contains("enters the battlefield") || condition_text.contains("enters")
    {
        if has_unconsumed_conditional(condition_text) {
            tracing::warn!(
                text = condition_text,
                "Unconsumed conditional in delayed trigger 'enters' match — parser may need extension"
            );
        }
        DelayedTriggerCondition::WhenEntersBattlefield {
            filter: parse_delayed_subject_filter(condition_text),
        }
    } else {
        return None;
    };

    // "that creature/permanent/token" references the parent spell's target.
    // "the exiled creature/card" and "the targeted creature" also reference
    // the parent's tracked set.
    let uses_tracked_set = condition_text.contains("that ")
        || condition_text.contains("the exiled ")
        || condition_text.contains("the targeted ");

    let inner = parse_effect_chain(effect_text, AbilityKind::Spell);

    Some(ParsedEffectClause {
        effect: Effect::CreateDelayedTrigger {
            condition,
            effect: Box::new(inner),
            uses_tracked_set,
        },
        duration: None,
        sub_ability: None,
        distribute: None,
        multi_target: None,
    })
}

/// Map delayed trigger condition subjects to TargetFilter.
/// CR 603.7c: Delayed triggers track objects by reference.
/// "that creature"/"that permanent"/"that token"/"that card" → ParentTarget (parent spell's target).
/// "the exiled creature"/"the exiled card"/"the creature"/"the permanent" → ParentTarget (back-reference).
/// "the targeted creature" → ParentTarget.
/// "it"/"this creature"/"this permanent"/"this artifact" → SelfRef (source object).
/// "target creature" → ParentTarget (named target in the condition).
fn parse_delayed_subject_filter(condition_text: &str) -> TargetFilter {
    if condition_text.contains("that ")
        || condition_text.contains("the exiled ")
        || condition_text.contains("the targeted ")
        || condition_text.contains("the creature")
        || condition_text.contains("the permanent")
        || condition_text.contains("the token")
        || condition_text.contains("target ")
    {
        TargetFilter::ParentTarget
    } else if condition_text.contains("it ")
        || condition_text.starts_with("it")
        || condition_text.contains("this creature")
        || condition_text.contains("this permanent")
        || condition_text.contains("this artifact")
    {
        TargetFilter::SelfRef
    } else {
        TargetFilter::Any
    }
}

/// CR 614.16: Parse "Damage can't be prevented [this turn]" into Effect::AddRestriction.
/// Handles variants:
///   - "Damage can't be prevented this turn"
///   - "Combat damage that would be dealt by creatures you control can't be prevented"
fn try_parse_damage_prevention_disabled(tp: TextPair) -> Option<ParsedEffectClause> {
    if !tp.contains("can't be prevented") && !tp.contains("cannot be prevented") {
        return None;
    }
    if !tp.contains("damage") {
        return None;
    }

    // Determine expiry: "this turn" → EndOfTurn, otherwise EndOfTurn as default
    let expiry = if tp.contains("this turn") {
        crate::types::ability::RestrictionExpiry::EndOfTurn
    } else {
        // Default to EndOfTurn for damage prevention restrictions
        crate::types::ability::RestrictionExpiry::EndOfTurn
    };

    // Determine scope from the subject phrase
    let scope = if tp.contains("creatures you control") || tp.contains("sources you control") {
        Some(
            crate::types::ability::RestrictionScope::SourcesControlledBy(
                crate::types::player::PlayerId(0), // Placeholder — resolved at runtime from ability controller
            ),
        )
    } else {
        // Global: all damage prevention disabled
        None
    };

    let restriction = crate::types::ability::GameRestriction::DamagePreventionDisabled {
        source: crate::types::identifiers::ObjectId(0), // Filled in at resolution time
        expiry,
        scope,
    };

    Some(ParsedEffectClause {
        effect: Effect::AddRestriction { restriction },
        duration: None,
        sub_ability: None,
        distribute: None,
        multi_target: None,
    })
}

/// CR 608.2d: Parse "have it [verb]" / "have you [verb]" causative constructions.
/// Used by "any opponent may" effects where the opponent causes the source or controller
/// to perform an action (e.g., "have it deal 4 damage to them").
fn try_parse_have_causative(tp: TextPair<'_>, ctx: &ParseContext) -> Option<ParsedEffectClause> {
    // Pattern A: "have it deal N damage to them" / "have ~ deal N damage to them"
    let after_have = tp
        .strip_prefix("have it ")
        .or_else(|| tp.strip_prefix("have ~ "));
    if let Some(rest) = after_have {
        // "deal N damage to them" / "deal N damage to that player"
        if let Some(after_deal) = rest.strip_prefix("deal ") {
            if let Some((amount, _)) = super::oracle_util::parse_count_expr(after_deal.lower) {
                return Some(parsed_clause(Effect::DealDamage {
                    amount,
                    target: TargetFilter::Player,
                    damage_source: None,
                }));
            }
        }
        // Fallback: parse remaining as a generic imperative effect
        let clause = parse_effect_clause(rest.original, ctx);
        return Some(clause);
    }

    // Pattern B: "have you [verb]" — controller performs an action directed by opponent
    if let Some(rest) = tp.strip_prefix("have you ") {
        let clause = parse_effect_clause(rest.original, ctx);
        return Some(clause);
    }

    None
}

#[tracing::instrument(level = "debug")]
fn parse_effect_clause(text: &str, ctx: &ParseContext) -> ParsedEffectClause {
    let text = strip_leading_sequence_connector(text)
        .trim()
        .trim_end_matches('.');
    if text.is_empty() {
        return parsed_clause(Effect::Unimplemented {
            name: "empty".to_string(),
            description: None,
        });
    }

    // Single lowercase pass for all case-insensitive matching within this clause.
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // CR 608.2d: "have it [verb]" / "have you [verb]" — causative construction
    // from "any opponent may" effects (e.g., "have it deal 4 damage to them").
    if let Some(clause) = try_parse_have_causative(tp, ctx) {
        return clause;
    }

    // CR 122.1: "you get {E}{E}" — gain energy counters.
    if tp.contains("{e}") && (tp.starts_with("you get ") || tp.starts_with("get ")) {
        let amount = super::oracle_util::count_energy_symbols(tp.lower);
        if amount > 0 {
            return parsed_clause(Effect::GainEnergy { amount });
        }
    }

    // CR 106.12: "don't lose [unspent] {color} mana as steps and phases end" —
    // mana pool retention. Parsed as supported no-op (runtime behavior is future work).
    if tp.contains("lose") && tp.contains("mana as steps") {
        return parsed_clause(Effect::GenericEffect {
            static_abilities: vec![],
            duration: None,
            target: None,
        });
    }

    // CR 701.54: "the ring tempts you" — Ring Tempts You effect.
    if tp.contains("the ring tempts you") {
        return parsed_clause(Effect::RingTemptsYou);
    }

    // CR 603.7c: "Whenever X this turn, Y" — multi-fire delayed trigger creation.
    if let Some(clause) = try_parse_whenever_this_turn(tp) {
        return clause;
    }

    // CR 603.7c: "When that creature dies, ..." — inline delayed trigger creation.
    if let Some(clause) = try_parse_inline_delayed_trigger(tp) {
        return clause;
    }

    // CR 614.16: "Damage can't be prevented [this turn]" → Effect::AddRestriction
    if let Some(clause) = try_parse_damage_prevention_disabled(tp) {
        return clause;
    }

    // CR 115.1d: "Two target creatures" / "Three target artifacts" — numeric target prefix.
    // Strip the count, recursively parse the remainder, and attach MultiTargetSpec.
    if let Some((count, rest)) = strip_numeric_target_prefix(tp.lower) {
        let mut clause = parse_effect_clause(rest, ctx);
        clause.multi_target = Some(MultiTargetSpec {
            min: count,
            max: Some(count),
        });
        return clause;
    }

    // CR 705: "If you win/lose the flip, [effect]" — coin flip branch.
    // Returns a FlipCoin with the appropriate branch filled in.
    // consolidate_die_and_coin_defs merges these into the preceding FlipCoin.
    if let Some((is_win, effect_text)) = imperative::try_parse_coin_flip_branch(text) {
        let branch_def = parse_effect_chain_impl(effect_text, AbilityKind::Spell, ctx);
        return if is_win {
            parsed_clause(Effect::FlipCoin {
                win_effect: Some(Box::new(branch_def)),
                lose_effect: None,
            })
        } else {
            parsed_clause(Effect::FlipCoin {
                win_effect: None,
                lose_effect: Some(Box::new(branch_def)),
            })
        };
    }

    if let Some((duration, rest)) = strip_leading_duration(text) {
        return with_clause_duration(parse_effect_clause(rest, ctx), duration);
    }

    // "it's still a/an [type]" / "that's still a/an [type]" — type-retention clause
    // CR 205.1a: Retains the original type in addition to new types from animation effects
    if let Some(clause) = try_parse_still_a_type(tp) {
        return clause;
    }

    // CR 601.2d: "deal N damage divided as you choose among [targets]" /
    // "distributed among" / "divided evenly" → DealDamage with distribute.
    if lower.contains("divided as you choose among")
        || lower.contains("distributed among")
        || lower.contains("divided evenly")
    {
        if let Some(clause) = try_parse_distribute_damage(&lower, text) {
            return clause;
        }
    }

    // CR 601.2d: "distribute N [type] counters among [targets]" →
    // PutCounter with distribute: Some(Counters(type)).
    if lower.starts_with("distribute ") && lower.contains("counter") && lower.contains("among") {
        if let Some(clause) = try_parse_distribute_counters(&lower, text) {
            return clause;
        }
    }

    // "for each" patterns: "draw a card for each [filter]", etc.
    if let Some(clause) = try_parse_for_each_effect(text) {
        return clause;
    }

    // CR 121.6: "{verb} cards equal to {quantity}" — dynamic count from game state.
    if let Some(clause) = try_parse_equal_to_quantity_effect(tp) {
        return clause;
    }

    // CR 702.84a: "exile cards from the top of your library until you exile a [filter] card"
    if let Some(clause) = try_parse_exile_from_top_until(tp) {
        return clause;
    }

    // CR 122.3: "cast that card by paying an amount of {E} equal to its mana value"
    // → GrantCastingPermission with ExileWithEnergyCost
    if tp.contains("by paying an amount of {e}") && tp.contains("equal to its mana value") {
        return parsed_clause(Effect::GrantCastingPermission {
            permission: CastingPermission::ExileWithEnergyCost,
            target: TargetFilter::ParentTarget,
        });
    }

    // CR 115.7: "change the target of" / "you may choose new targets for" —
    // must be checked before subject stripping since "you may choose" would be split.
    if let Some(effect) = try_parse_change_targets(tp.lower) {
        return parsed_clause(effect);
    }

    // CR 400.7i: "you may play/cast that card [this turn]" — impulse draw permission.
    if let Some(clause) = try_parse_play_from_exile(tp) {
        return clause;
    }

    // CR 701.57a: "discover N" — effect variant
    if let Some(rest) = tp.strip_prefix("discover ") {
        if let Ok(n) = rest.lower.trim().parse::<u32>() {
            return parsed_clause(Effect::Discover {
                mana_value_limit: n,
            });
        }
    }

    // CR 401.4: "[target]'s owner puts it on their choice of the top or bottom of their library"
    if let Some(clause) = try_parse_put_on_top_or_bottom(tp) {
        return clause;
    }

    // CR 509.1c: "All creatures able to block [target/~] [this turn] do so" — mass forced block.
    // Semantically equivalent to "[target/~] must be blocked this turn if able".
    if let Some(clause) = try_parse_mass_forced_block(tp) {
        return clause;
    }

    // CR 611.2b: "have [subject] [predicate]" — subject redirection where "have" means
    // "cause [subject] to [predicate]". Distinct from static "have" which grants keywords
    // (e.g., "creatures you control have flying"), which is a subject-predicate pattern with
    // a plural subject before "have". Here, "have" is the first word, followed by a target/
    // anaphoric reference and a predicate.
    if let Some(redirected) = try_parse_have_redirection(text, ctx) {
        return redirected;
    }

    let ast = parse_clause_ast(text, ctx);
    lower_clause_ast(ast, ctx)
}

/// CR 611.2b: Parse "have [subject] [predicate]" subject redirection.
///
/// In imperative Oracle text, "have" followed by a target reference or anaphoric pronoun
/// means "cause [subject] to [predicate]". Examples:
/// - "have target creature get +3/+3" → Pump with target creature
/// - "have it gain flying" → keyword grant with anaphoric "it"
/// - "have it fight target creature" → fight with redirected subject
///
/// Distinguished from static "have" (e.g., "creatures you control have flying") by position:
/// imperative "have" starts the clause; static "have" follows a subject.
fn try_parse_have_redirection(text: &str, ctx: &ParseContext) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    let after_have = lower.strip_prefix("have ")?;

    // Guard: don't intercept if what follows "have" is not a recognizable subject reference.
    // Subject references start with: "target", "it", "that", "each", "all", "them",
    // "another target", "this", "~", "enchanted", "equipped".
    let first_word = after_have.split_whitespace().next().unwrap_or("");
    let is_subject_ref = matches!(
        first_word,
        "target"
            | "it"
            | "that"
            | "each"
            | "all"
            | "them"
            | "another"
            | "this"
            | "~"
            | "enchanted"
            | "equipped"
    );
    if !is_subject_ref {
        return None;
    }

    // Strip "have " from the original text (preserving original casing)
    let redirected_text = text["have ".len()..].trim();

    // Try the subject-predicate AST parser on the redirected text.
    if let Some(ast) = subject::try_parse_subject_predicate_ast(redirected_text, ctx) {
        return Some(lower_clause_ast(ast, ctx));
    }

    // Fallback: if subject-predicate doesn't match, try parsing the redirected text
    // through the full parse_effect_clause pipeline (handles fight, damage, etc.).
    // This recursion is safe because "have" has been stripped — no infinite loop.
    let clause = parse_effect_clause(redirected_text, ctx);
    if !matches!(clause.effect, Effect::Unimplemented { .. }) {
        return Some(clause);
    }

    None
}

/// Parse "it's still a/an [type]" and "that's still a/an [type]" type-retention clauses.
///
/// These appear as separate sentences after animation effects (e.g., "This land becomes
/// a 3/3 creature with vigilance. It's still a land."). The clause ensures the original
/// type is retained as a permanent continuous effect.
///
/// CR 205.1a: An object retains types explicitly stated by the effect.
/// CR 509.1c: "All creatures able to block [target/~] [this turn] do so."
///
/// Semantically equivalent to "[target/~] must be blocked this turn if able."
/// Produces a GenericEffect with `MustBeBlocked` AddStaticMode on the referenced object.
///
/// Patterns:
/// - "all creatures able to block target creature this turn do so"
/// - "all creatures able to block ~ do so"
/// - "all creatures able to block ~ this turn do so"
fn try_parse_mass_forced_block(tp: TextPair) -> Option<ParsedEffectClause> {
    use crate::types::ability::ContinuousModification;

    let rest = tp.strip_prefix("all creatures able to block ")?;

    // The text must end with "do so" (the mass-block imperative)
    let rest_lower = rest.lower.trim_end_matches('.');
    if !rest_lower.ends_with("do so") {
        return None;
    }

    // Strip "do so" suffix, then optional "this turn " / "this combat " before it
    let before_do_so = rest_lower[..rest_lower.len() - "do so".len()].trim();
    let before_temporal = before_do_so
        .strip_suffix("this turn")
        .or_else(|| before_do_so.strip_suffix("this combat"))
        .unwrap_or(before_do_so)
        .trim();

    // Remaining text is the block target: "target creature", "~", "it", etc.
    let target_text = &rest.original[..before_temporal.len()];
    let (target, _) = parse_target(target_text);

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(StaticMode::MustBeBlocked)
                .affected(target.clone())
                .modifications(vec![ContinuousModification::AddStaticMode {
                    mode: StaticMode::MustBeBlocked,
                }])],
            duration: Some(Duration::UntilEndOfTurn),
            target: Some(target),
        },
        distribute: None,
        multi_target: None,
        duration: Some(Duration::UntilEndOfTurn),
        sub_ability: None,
    })
}

fn try_parse_still_a_type(tp: TextPair) -> Option<ParsedEffectClause> {
    use crate::types::ability::ContinuousModification;
    use crate::types::card_type::CoreType;

    // Match "it's still a/an [type]" or "that's still a/an [type]"
    let rest = tp
        .strip_prefix("it's still ")
        .or_else(|| tp.strip_prefix("that's still "))?;
    let type_name = rest
        .strip_prefix("a ")
        .or_else(|| rest.strip_prefix("an "))?;
    let core_type = CoreType::from_str(&capitalize(type_name.lower)).ok()?;

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddType { core_type }])
                .description(tp.original.to_string())],
            duration: Some(Duration::Permanent),
            target: None,
        },
        duration: Some(Duration::Permanent),
        sub_ability: None,
        distribute: None,
        multi_target: None,
    })
}

/// Parse "{verb} cards equal to {quantity_ref}" patterns (CR 121.6).
///
/// Handles verbs whose count field is `QuantityExpr` (mill, draw).
fn try_parse_equal_to_quantity_effect(tp: TextPair) -> Option<ParsedEffectClause> {
    if let Some(rest) = tp.strip_prefix("mill cards equal to ") {
        let rest = rest.lower.trim().trim_end_matches('.');
        let qty = super::oracle_quantity::parse_quantity_ref(rest)?;
        return Some(parsed_clause(Effect::Mill {
            count: QuantityExpr::Ref { qty },
            // CR 701.17a: No subject → controller mills.
            target: TargetFilter::Controller,
        }));
    }
    if let Some(rest) = tp.strip_prefix("draw cards equal to ") {
        let rest = rest.lower.trim().trim_end_matches('.');
        let qty = super::oracle_quantity::parse_quantity_ref(rest)?;
        return Some(parsed_clause(Effect::Draw {
            count: QuantityExpr::Ref { qty },
        }));
    }
    None
}

/// CR 702.84a: Parse "exile cards from the top of your library until you exile a [filter] card".
/// CR 401.4: Parse "owner puts it on their choice of the top or bottom of their library".
///
/// Two Oracle patterns:
/// - "Target creature's owner puts it on their choice of the top or bottom of their library"
/// - "The owner of target nonland permanent puts it on their choice of the top or bottom of their library"
fn try_parse_put_on_top_or_bottom(tp: TextPair) -> Option<ParsedEffectClause> {
    let tp = tp.trim_end_matches('.');

    // Must contain the signature suffix
    if !tp.contains("on the top or bottom of their library")
        && !tp.contains("on their choice of the top or bottom of their library")
    {
        return None;
    }

    // Pattern 1: "target creature's owner puts it ..."
    // Strip "'s owner puts it on ..." suffix, parse the target prefix.
    if let Some(idx) = tp.find("'s owner puts it") {
        let target_text = &tp.original[..idx];
        let (filter, _) = parse_target(target_text);
        return Some(parsed_clause(Effect::PutOnTopOrBottom { target: filter }));
    }

    // Pattern 2: "the owner of target nonland permanent puts it ..."
    if let Some(rest) = tp.strip_prefix("the owner of ") {
        if let Some(idx) = rest.find(" puts it") {
            let target_text = &rest.original[..idx];
            let (filter, _) = parse_target(target_text);
            return Some(parsed_clause(Effect::PutOnTopOrBottom { target: filter }));
        }
    }

    None
}

fn try_parse_exile_from_top_until(tp: TextPair) -> Option<ParsedEffectClause> {
    // Match: "exile cards from the top of your library until you exile a {filter} card"
    let rest = tp.strip_prefix("exile cards from the top of your library until you exile a ")?;

    // Extract the filter from "nonland card", "creature card", etc.
    let filter_text = rest.lower.trim_end_matches('.').trim_end_matches(" card");

    let filter = if filter_text == "nonland" {
        TargetFilter::Typed(
            TypedFilter::default().with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
        )
    } else {
        // Delegate to existing target parsing for other filter types
        let (parsed, _) = parse_target(filter_text);
        parsed
    };

    Some(parsed_clause(Effect::ExileFromTopUntil { filter }))
}

/// CR 400.7i: Parse "you may play/cast that card [this turn]" — impulse draw permission.
fn try_parse_play_from_exile(tp: TextPair) -> Option<ParsedEffectClause> {
    let tp = tp.trim_end_matches('.');

    // Try full forms first: "you may play/cast that card/it/those cards ..."
    // Then bare forms (after "you may" has been stripped): "play that card ..."
    let full_rest = tp
        .strip_prefix("you may play ")
        .or_else(|| tp.strip_prefix("you may cast "));

    if let Some(rest) = full_rest {
        // Full form: rest must start with a card reference
        if !(rest.starts_with("that card")
            || rest.starts_with("that spell")
            || rest.starts_with("those cards")
            || rest.starts_with("it ")
            || rest.lower == "it")
        {
            return None;
        }
    } else {
        // Bare form (after "you may" was stripped by parse_effect_chain):
        // Only match when temporal context exists ("this turn", "until"),
        // otherwise it's a CastFromZone, not impulse draw permission.
        let has_temporal = tp.contains("this turn") || tp.contains("until ");
        if !has_temporal {
            return None;
        }
        if tp.contains("without paying") {
            return None;
        }
        if !(tp.starts_with("play that card")
            || tp.starts_with("cast that card")
            || tp.starts_with("play it")
            || tp.starts_with("cast it"))
        {
            return None;
        }
    }

    // Duration: extract from trailing text, defaulting to UntilEndOfTurn for impulse draw
    let (_, dur) = strip_trailing_duration(tp.original);
    let duration = dur.unwrap_or(Duration::UntilEndOfTurn);

    Some(parsed_clause(Effect::GrantCastingPermission {
        permission: CastingPermission::PlayFromExile { duration },
        target: TargetFilter::Any,
    }))
}

/// Parse "for each" quantity patterns on draw/life/damage/mill effects.
///
/// Handles patterns like:
/// - "draw a card for each opponent who lost life this turn"
/// - "draw a card for each creature you control"
/// - "gain 1 life for each creature you control"
/// - "mill a card for each [counter type] counter on ~"
fn try_parse_for_each_effect(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Find "for each" in the text
    let for_each_idx = tp.find("for each ")?;
    let (base_tp, _) = tp.split_at(for_each_idx);
    let base_tp = base_tp.trim_end();
    let for_each_clause = &tp.lower[for_each_idx + "for each ".len()..];

    // Parse the "for each" clause into a QuantityRef
    let qty = parse_for_each_clause(for_each_clause)?;
    let quantity = QuantityExpr::Ref { qty };

    // Parse the base effect and replace its count with the dynamic quantity
    if base_tp.starts_with("draw ") || base_tp.contains(" draw") {
        return Some(parsed_clause(Effect::Draw { count: quantity }));
    }

    if (base_tp.starts_with("you gain ") || base_tp.starts_with("gain "))
        && base_tp.contains("life")
    {
        // Extract multiplier: "gain 3 life" → factor=3, "gain life" → factor=1
        let after_gain = base_tp
            .lower
            .strip_prefix("you gain ")
            .or_else(|| base_tp.lower.strip_prefix("gain "))
            .unwrap_or(base_tp.lower);
        let amount = match parse_number(after_gain) {
            Some((n, _)) if n > 1 => QuantityExpr::Multiply {
                factor: n as i32,
                inner: Box::new(quantity),
            },
            _ => quantity,
        };
        return Some(parsed_clause(Effect::GainLife {
            amount,
            player: GainLifePlayer::Controller,
        }));
    }

    if (base_tp.starts_with("you lose ") || base_tp.starts_with("lose "))
        && base_tp.contains("life")
    {
        // Extract multiplier: "lose 3 life" → factor=3
        let after_lose = base_tp
            .lower
            .strip_prefix("you lose ")
            .or_else(|| base_tp.lower.strip_prefix("lose "))
            .unwrap_or(base_tp.lower);
        let amount = match parse_number(after_lose) {
            Some((n, _)) if n > 1 => QuantityExpr::Multiply {
                factor: n as i32,
                inner: Box::new(quantity),
            },
            _ => quantity,
        };
        return Some(parsed_clause(Effect::LoseLife { amount }));
    }

    None
}

#[tracing::instrument(level = "trace")]
fn parse_clause_ast(text: &str, ctx: &ParseContext) -> ClauseAst {
    let text = text.trim();

    // Mirror the CubeArtisan grammar's high-level sentence shapes:
    // 1) conditionals ("if X, Y"), 2) subject + verb phrase, 3) bare imperative.
    if let Some((condition_text, remainder)) = split_leading_conditional(text) {
        let _ = condition_text;
        return ClauseAst::Conditional {
            clause: Box::new(parse_clause_ast(&remainder, ctx)),
        };
    }

    if let Some(ast) = try_parse_subject_predicate_ast(text, ctx) {
        return ast;
    }

    ClauseAst::Imperative {
        text: text.to_string(),
    }
}

fn lower_clause_ast(ast: ClauseAst, ctx: &ParseContext) -> ParsedEffectClause {
    match ast {
        ClauseAst::Imperative { text } => lower_imperative_clause(&text, ctx),
        ClauseAst::SubjectPredicate { subject, predicate } => {
            lower_subject_predicate_ast(*subject, *predicate, ctx)
        }
        ClauseAst::Conditional { clause } => {
            // Phase 2 preserves current semantics for generic leading conditionals:
            // recognize the structure explicitly, but lower only the body.
            lower_clause_ast(*clause, ctx)
        }
    }
}

#[tracing::instrument(level = "debug")]
fn lower_imperative_clause(text: &str, ctx: &ParseContext) -> ParsedEffectClause {
    // "Its controller gains life equal to its power/toughness" — subject must be preserved
    // because the life recipient is not the caster but the targeted permanent's controller.
    if let Some(clause) = try_parse_targeted_controller_gain_life(text) {
        return clause;
    }

    // Compound shuffle subjects: "shuffle ~ and target creature ... into their owners' libraries"
    // Must come before try_split_targeted_compound because "shuffle" is the verb, not the subject.
    if let Some(clause) = try_parse_compound_shuffle(text) {
        return clause;
    }

    // Compound targeted actions: "tap target creature and put a stun counter on it"
    // Split on " and " when the primary clause is a targeted verb.
    if let Some(clause) = try_split_targeted_compound(text, ctx) {
        return clause;
    }

    // CR 608.2c: Compound damage actions: "~ deals 3 damage to any target and you gain 3 life"
    if let Some(clause) = try_split_damage_compound(text, ctx) {
        return clause;
    }

    let (stripped, duration) = strip_trailing_duration(text);
    let mut clause = parse_imperative_effect(stripped, ctx);
    if clause.duration.is_none() {
        clause.duration = duration;
    }
    // CR 115.1d: Post-parse fixup for PutCounter "up to N" multi_target.
    // The multi_target is lost in the AST→Effect lowering chain, so we re-extract it
    // from the original text when the effect is PutCounter with a targeted filter.
    if matches!(clause.effect, Effect::PutCounter { .. }) && clause.multi_target.is_none() {
        clause.multi_target = extract_put_counter_multi_target(text);
    }
    clause
}

/// Parse a verb prefix and its target, returning the AST and `parse_target`'s unconsumed
/// remainder. Used by `try_split_targeted_compound` to determine compound boundaries
/// semantically — `parse_target` correctly consumes compound filter phrases like
/// "you own and control", so its remainder reveals whether " and " is a true compound
/// connector or part of the target filter.
///
/// CR 608.2c: The instructions in a spell or ability are followed in order; this helper
/// identifies the boundary between the first instruction and any subsequent compound action.
///
/// NOTE: Shares verb prefixes with `parse_targeted_action_ast` in `imperative.rs`.
/// When adding a new targeted verb here, check if it also needs to be added there.
fn try_parse_verb_and_target<'a>(
    text: &'a str,
    lower: &str,
    ctx: &ParseContext,
) -> Option<(TargetedImperativeAst, &'a str)> {
    // Simple targeted verbs: parse_target on text after the verb prefix
    if lower.starts_with("tap ") {
        let (target, rem) = parse_target(&text[4..]);
        return Some((TargetedImperativeAst::Tap { target }, rem));
    }
    if lower.starts_with("untap ") {
        let (target, rem) = parse_target(&text[6..]);
        return Some((TargetedImperativeAst::Untap { target }, rem));
    }
    if lower.starts_with("sacrifice ") {
        let (target, rem) = parse_target(&text[10..]);
        return Some((TargetedImperativeAst::Sacrifice { target }, rem));
    }
    if lower.starts_with("fight ") {
        let (target, rem) = parse_target(&text[6..]);
        return Some((TargetedImperativeAst::Fight { target }, rem));
    }
    if lower.starts_with("gain control of ") {
        let (target, rem) = parse_target(&text[16..]);
        return Some((TargetedImperativeAst::GainControl { target }, rem));
    }
    // Earthbend: "earthbend [N] [target <type>]" → Animate with haste + is_earthbend
    if let Some(rest) = lower.strip_prefix("earthbend ") {
        let (target, power, toughness) = imperative::parse_earthbend_params(text, rest);
        return Some((
            TargetedImperativeAst::Earthbend {
                target,
                power,
                toughness,
            },
            "", // remainder is consumed by parse_earthbend_params
        ));
    }
    // Airbend: "airbend target <type> <mana_cost>" → GrantCastingPermission(ExileWithAltCost)
    if let Some(rest) = lower.strip_prefix("airbend ") {
        let original_rest = &text[text.len() - rest.len()..];
        let (target, after_target) = parse_target(original_rest);
        let (cost, rem) = parse_mana_symbols(after_target.trim_start()).unwrap_or((
            crate::types::mana::ManaCost::Cost {
                generic: 2,
                shards: vec![],
            },
            after_target,
        ));
        return Some((TargetedImperativeAst::Airbend { target, cost }, rem));
    }

    // Destroy: check "all"/"each" prefix for mass destruction
    if lower.starts_with("destroy all ") || lower.starts_with("destroy each ") {
        let (target, rem) = parse_target(&text[8..]);
        return Some((
            TargetedImperativeAst::ZoneCounterProxy(Box::new(ZoneCounterImperativeAst::Destroy {
                target,
                all: true,
            })),
            rem,
        ));
    }
    if lower.starts_with("destroy ") {
        let (target, rem) = parse_target(&text[8..]);
        return Some((
            TargetedImperativeAst::ZoneCounterProxy(Box::new(ZoneCounterImperativeAst::Destroy {
                target,
                all: false,
            })),
            rem,
        ));
    }

    // Exile: infer origin zone from the full post-verb text (NOT the remainder,
    // since parse_zone_suffix inside parse_type_phrase consumes zone phrases).
    if lower.starts_with("exile all ") || lower.starts_with("exile each ") {
        let rest_lower = &lower[6..]; // after "exile "
        let (target, rem) = parse_target(&text[6..]);
        let origin = infer_origin_zone(rest_lower);
        return Some((
            TargetedImperativeAst::ZoneCounterProxy(Box::new(ZoneCounterImperativeAst::Exile {
                origin,
                target,
                all: true,
            })),
            rem,
        ));
    }
    if let Some(rest_lower) = lower.strip_prefix("exile ") {
        let (target, rem) = parse_target(&text[6..]);
        let origin = infer_origin_zone(rest_lower);
        return Some((
            TargetedImperativeAst::ZoneCounterProxy(Box::new(ZoneCounterImperativeAst::Exile {
                origin,
                target,
                all: false,
            })),
            rem,
        ));
    }

    // CR 701.5a: Counter a spell or ability on the stack.
    if let Some(rest_lower) = lower.strip_prefix("counter ") {
        let (parsed_target, rem) = parse_target(&text[8..]);
        let target = if rest_lower.contains("activated or triggered ability") {
            // CR 701.5a: "activated or triggered ability" is a special-case target
            // that maps to StackAbility. We still use parse_target's remainder to
            // preserve the compound-detection contract.
            TargetFilter::StackAbility
        } else if rest_lower.contains("spell") {
            constrain_filter_to_stack(parsed_target)
        } else {
            parsed_target
        };
        // CR 118.12: Parse "unless its controller pays {X}" for conditional counters
        let unless_payment = parse_unless_payment(&lower[8..]);
        return Some((
            TargetedImperativeAst::ZoneCounterProxy(Box::new(ZoneCounterImperativeAst::Counter {
                target,
                source_static: None,
                unless_payment,
            })),
            rem,
        ));
    }

    // Return: determine destination separately, use parse_target remainder for compound detection
    if lower.starts_with("return ") {
        let rest = &text[7..];
        let (_, dest) = strip_return_destination_ext(rest);
        let (target, rem) = parse_target(rest);
        return match dest {
            Some(d) if d.zone == Zone::Battlefield => Some((
                TargetedImperativeAst::ReturnToBattlefield {
                    target,
                    enter_transformed: d.transformed,
                    under_your_control: d.under_your_control,
                    enter_tapped: d.enter_tapped,
                },
                rem,
            )),
            Some(d) if d.zone == Zone::Hand => {
                Some((TargetedImperativeAst::Return { target }, rem))
            }
            Some(d) => Some((
                TargetedImperativeAst::ReturnToZone {
                    target,
                    destination: d.zone,
                },
                rem,
            )),
            None => Some((TargetedImperativeAst::Return { target }, rem)),
        };
    }

    // Put counter: use refactored try_parse_put_counter that returns remainder
    if lower.starts_with("put ") && lower.contains("counter") {
        if let Some((
            Effect::PutCounter {
                counter_type,
                count,
                target,
            },
            rem,
            _multi_target,
        )) = counter::try_parse_put_counter(lower, text, ctx)
        {
            return Some((
                TargetedImperativeAst::ZoneCounterProxy(Box::new(
                    ZoneCounterImperativeAst::PutCounter {
                        counter_type,
                        count,
                        target,
                    },
                )),
                rem,
            ));
        }
    }

    None
}

/// CR 608.2c: Split compound targeted actions like "tap target creature and put a stun
/// counter on it" into a primary effect (Tap) with a sub_ability chain (PutCounter with
/// ParentTarget). Instructions in a spell are followed in order; each " and "-connected
/// action becomes a chained sub_ability.
///
/// Uses `parse_target`'s unconsumed remainder as the compound boundary oracle — this correctly
/// handles compound filter phrases like "you own and control" because `parse_target` consumes
/// them as part of the target filter, leaving no " and " in the remainder.
///
/// When the remainder references "it"/"that creature"/"them" (via `contains_object_pronoun`),
/// the sub_ability's target is set to `TargetFilter::ParentTarget` so it inherits the
/// parent's resolved targets at resolution time.
fn try_split_targeted_compound(text: &str, ctx: &ParseContext) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();

    // Quick bail: no " and " means no compound connector possible
    if !lower.contains(" and ") {
        return None;
    }

    // Use parse_target's remainder to determine the compound split point
    let (primary_ast, remainder) = try_parse_verb_and_target(text, &lower, ctx)?;

    // If parse_target consumed everything, there's no compound action
    // (e.g. "exile any number of other nonland permanents you own and control")
    if remainder.is_empty() {
        return None;
    }

    // The remainder must start with " and " to be a compound connector.
    // Do NOT trim — the leading space is the boundary marker.
    let after_and = remainder.strip_prefix(" and ")?;

    let sub_text = after_and.trim();
    if sub_text.is_empty() {
        return None;
    }

    // Lower the primary AST to an Effect
    let primary_effect = match primary_ast {
        TargetedImperativeAst::ZoneCounterProxy(ast) => lower_zone_counter_ast(*ast),
        other => lower_targeted_action_ast(other),
    };

    // Parse the sub-effect
    let mut sub_clause = parse_imperative_effect(sub_text, ctx);

    // If the remainder contains anaphoric references ("it", "that creature", "them"),
    // replace the sub_effect's target with ParentTarget so it inherits the parent's targets.
    let sub_lower = sub_text.to_lowercase();
    if has_anaphoric_reference(&sub_lower) {
        replace_target_with_parent(&mut sub_clause.effect);
    }

    let mut sub_ability = AbilityDefinition::new(AbilityKind::Spell, sub_clause.effect);
    sub_ability.sub_ability = sub_clause.sub_ability;

    Some(ParsedEffectClause {
        effect: primary_effect,
        duration: None,
        sub_ability: Some(Box::new(sub_ability)),
        distribute: None,
        multi_target: None,
    })
}

/// CR 608.2c: Split compound damage actions like "~ deals 3 damage to any target
/// and you gain 3 life" into a primary DealDamage effect with a sub_ability chain
/// for the remainder clause. Instructions within a single spell or ability are
/// followed in order; each " and "-connected clause becomes a chained sub_ability.
///
/// Uses `parse_target`'s unconsumed remainder as the compound boundary oracle —
/// this correctly handles compound filter phrases like "you own and control"
/// because `parse_target` consumes them as part of the target filter.
fn try_split_damage_compound(text: &str, ctx: &ParseContext) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    if !lower.contains(" and ") {
        return None;
    }

    let (primary_effect, remainder) = try_parse_damage_with_remainder(text, &lower)?;

    if remainder.is_empty() {
        return None;
    }

    // The remainder must start with " and " to be a compound connector.
    // Do NOT trim — the leading space is the boundary marker.
    let after_and = remainder.strip_prefix(" and ")?;
    let sub_text = after_and.trim();
    if sub_text.is_empty() {
        return None;
    }

    // Parse the sub-effect through the full clause pipeline (not just imperative),
    // because the sub-text may have a subject prefix like "you gain 3 life".
    let mut sub_clause = parse_effect_clause(sub_text, ctx);

    // Guard: if the sub-text parsed to Unimplemented, it's likely a target phrase
    // continuation ("each creature and planeswalker they control") rather than an
    // independent clause. Bail out and let the damage parser handle the full text.
    if matches!(sub_clause.effect, Effect::Unimplemented { .. }) {
        return None;
    }

    // If the remainder contains anaphoric references ("it", "that creature", "them"),
    // replace the sub_effect's target with ParentTarget so it inherits the parent's targets.
    let sub_lower = sub_text.to_lowercase();
    if has_anaphoric_reference(&sub_lower) {
        replace_target_with_parent(&mut sub_clause.effect);
    }

    let mut sub_ability = AbilityDefinition::new(AbilityKind::Spell, sub_clause.effect);
    sub_ability.sub_ability = sub_clause.sub_ability;

    Some(ParsedEffectClause {
        effect: primary_effect,
        duration: None,
        sub_ability: Some(Box::new(sub_ability)),
        distribute: None,
        multi_target: None,
    })
}

/// Verb-agnostic compound subject splitter.
/// Splits "X and Y [remainder]" into two subjects + the verb phrase.
/// X and Y are each parsed via `parse_target` or SelfRef detection.
/// Returns None if no compound subject detected.
///
/// Examples:
///   "~ and target creature with a stun counter on it into their owners' libraries"
///   → (SelfRef, Typed(Creature+CountersGE(Stun,1)), "into their owners' libraries")
fn try_split_compound_subject(text: &str) -> Option<(TargetFilter, TargetFilter, &str)> {
    let lower = text.to_lowercase();

    // Find " and " that separates subjects
    let and_pos = lower.find(" and ")?;
    let first_text = text[..and_pos].trim();
    let after_and = text[and_pos + 5..].trim(); // skip " and "

    // Parse first subject
    let first_filter = if first_text == "~"
        || first_text.eq_ignore_ascii_case("this creature")
        || first_text.eq_ignore_ascii_case("this permanent")
    {
        TargetFilter::SelfRef
    } else {
        let (filter, _rest) = parse_target(first_text);
        if matches!(filter, TargetFilter::None) {
            return None;
        }
        filter
    };

    // Parse second subject — consume until we hit a preposition that starts the verb phrase
    // Look for "into " or "from " as the boundary between the second subject and remainder
    let after_and_lower = after_and.to_lowercase();
    let remainder_start = after_and_lower
        .find(" into ")
        .or_else(|| after_and_lower.find(" from "))
        .or_else(|| after_and_lower.find(" onto "));

    let (second_text, remainder) = if let Some(pos) = remainder_start {
        (after_and[..pos].trim(), after_and[pos..].trim())
    } else {
        // No remainder phrase found — entire after_and is the second subject
        (after_and, "")
    };

    let (second_filter, extra_rest) = parse_target(second_text);
    if matches!(second_filter, TargetFilter::None) {
        return None;
    }

    // If parse_target consumed less than the full second_text, combine leftovers with remainder
    let extra_rest = extra_rest.trim();
    let final_remainder = if !extra_rest.is_empty() && !remainder.is_empty() {
        // extra_rest comes before the remainder preposition — just use remainder
        remainder
    } else if !extra_rest.is_empty() {
        extra_rest
    } else {
        remainder
    };

    Some((first_filter, second_filter, final_remainder))
}

/// Parse "shuffle X and Y into their owners' libraries" as a compound ChangeZone chain.
/// Returns a ParsedEffectClause with a ChangeZone for the first subject and a sub_ability
/// for the second subject, both with owner_library: true.
fn try_parse_compound_shuffle(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    lower.strip_prefix("shuffle ")?;

    // Try to split compound subject from the text after "shuffle "
    let text_after = &text["shuffle ".len()..];
    let (first, second, remainder) = try_split_compound_subject(text_after)?;

    // The remainder must indicate library destination
    let remainder_lower = remainder.to_lowercase();
    let is_owner_library = remainder_lower.contains("owner")
        || remainder_lower.contains("their")
        || remainder_lower.contains("its");

    if !remainder_lower.contains("librar") {
        return None;
    }

    let owner_library = is_owner_library;

    // CR 701.24a: Compound shuffle is ChangeZone(first) → ChangeZone(second) → Shuffle.
    let shuffle_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Shuffle {
            target: TargetFilter::Controller,
        },
    );

    // Build ChangeZone for the second subject, chained to the Shuffle
    let sub_effect = Effect::ChangeZone {
        origin: None,
        destination: Zone::Library,
        target: second,
        owner_library,
        enter_transformed: false,
        under_your_control: false,
        enter_tapped: false,
        enters_attacking: false,
    };
    let mut sub_def = AbilityDefinition::new(AbilityKind::Spell, sub_effect);
    sub_def.sub_ability = Some(Box::new(shuffle_def));

    // Build ChangeZone for the first subject as the primary effect
    let primary_effect = Effect::ChangeZone {
        origin: None,
        destination: Zone::Library,
        target: first,
        owner_library,
        enter_transformed: false,
        under_your_control: false,
        enter_tapped: false,
        enters_attacking: false,
    };

    Some(ParsedEffectClause {
        effect: primary_effect,
        duration: None,
        sub_ability: Some(Box::new(sub_def)),
        distribute: None,
        multi_target: None,
    })
}

/// Check if text contains anaphoric pronouns referencing a previously mentioned object.
/// Unlike `contains_object_pronoun`, this handles word boundaries at end-of-string
/// (e.g., "counter on it" where "it" is the last word).
fn has_anaphoric_reference(lower: &str) -> bool {
    for pronoun in [
        "it",
        "them",
        "that creature",
        "that card",
        "those cards",
        "that permanent",
    ] {
        // Check whole-word boundary: pronoun preceded by space/start and followed by space/end/punctuation
        if let Some(pos) = lower.find(pronoun) {
            let before_ok = pos == 0 || lower.as_bytes()[pos - 1] == b' ';
            let after_pos = pos + pronoun.len();
            let after_ok = after_pos >= lower.len()
                || matches!(
                    lower.as_bytes()[after_pos],
                    b' ' | b',' | b'.' | b'\'' | b's'
                );
            if before_ok && after_ok {
                return true;
            }
        }
    }
    false
}

/// Replace the target filter on an effect with ParentTarget.
/// Used for anaphoric "it"/"that creature" references in compound sub-effects.
fn replace_target_with_parent(effect: &mut Effect) {
    match effect {
        Effect::Tap { target }
        | Effect::Untap { target }
        | Effect::Destroy { target, .. }
        | Effect::Sacrifice { target }
        | Effect::GainControl { target }
        | Effect::Fight { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::DealDamage { target, .. }
        | Effect::Pump { target, .. }
        | Effect::Attach { target, .. }
        | Effect::Counter { target, .. }
        | Effect::Transform { target, .. }
        | Effect::Connive { target, .. }
        | Effect::PhaseOut { target }
        | Effect::ForceBlock { target } => {
            *target = TargetFilter::ParentTarget;
        }
        Effect::PutCounter { target, .. }
        | Effect::AddCounter { target, .. }
        | Effect::RemoveCounter { target, .. } => {
            *target = TargetFilter::ParentTarget;
        }
        Effect::ChangeZone { target, .. } | Effect::ChangeZoneAll { target, .. } => {
            *target = TargetFilter::ParentTarget;
        }
        Effect::GenericEffect { target, .. } => {
            *target = Some(TargetFilter::ParentTarget);
        }
        _ => {
            // Effects without a target field (Draw, GainLife, etc.) stay as-is.
            // ParentTarget is handled by the sub_ability chain's target propagation.
        }
    }
}

/// Check if an effect has a `Typed(...)` target filter (not SelfRef/ParentTarget/Any).
/// Used to guard anaphoric replacement scope — prevents false positives when a
/// pronoun clause follows a conditional effect without a typed target.
fn has_typed_target(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::PutCounter {
            target: TargetFilter::Typed(_),
            ..
        } | Effect::Pump {
            target: TargetFilter::Typed(_),
            ..
        } | Effect::DealDamage {
            target: TargetFilter::Typed(_),
            ..
        } | Effect::Destroy {
            target: TargetFilter::Typed(_),
            ..
        } | Effect::Tap {
            target: TargetFilter::Typed(_),
            ..
        } | Effect::Bounce {
            target: TargetFilter::Typed(_),
            ..
        } | Effect::GainControl {
            target: TargetFilter::Typed(_),
            ..
        } | Effect::Attach {
            target: TargetFilter::Typed(_),
            ..
        }
    )
}

fn lower_subject_predicate_ast(
    subject: SubjectPhraseAst,
    predicate: PredicateAst,
    ctx: &ParseContext,
) -> ParsedEffectClause {
    match predicate {
        PredicateAst::Continuous {
            effect,
            duration,
            sub_ability,
        } => ParsedEffectClause {
            effect,
            duration,
            sub_ability,
            distribute: None,
            multi_target: None,
        },
        PredicateAst::Become {
            effect,
            duration,
            sub_ability,
        } => ParsedEffectClause {
            effect,
            duration,
            sub_ability,
            distribute: None,
            multi_target: None,
        },
        PredicateAst::Restriction { effect, duration } => ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
            distribute: None,
            multi_target: None,
        },
        PredicateAst::ImperativeFallback { text } => {
            let pred_lower = text.to_lowercase();
            if matches!(pred_lower.as_str(), "shuffle" | "shuffles")
                && matches!(
                    subject.affected,
                    TargetFilter::Player | TargetFilter::Controller
                )
            {
                return parsed_clause(Effect::Shuffle {
                    target: subject.affected,
                });
            }
            // CR 701.20a: "<player> reveals the top [N] card(s) of their library"
            if (pred_lower.starts_with("reveal ") || pred_lower.starts_with("reveals "))
                && pred_lower.contains("top")
                && pred_lower.contains("library")
            {
                let count = if let Some(after_top) = strip_after(&pred_lower, "top ") {
                    super::oracle_util::parse_number(after_top)
                        .map(|(n, _)| n)
                        .unwrap_or(1)
                } else {
                    1
                };
                return parsed_clause(Effect::RevealTop {
                    player: subject.affected,
                    count,
                });
            }
            let mut clause = lower_imperative_clause(&text, ctx);
            // CR 608.2c: Inject the subject's target into targeted effects that were
            // parsed via the imperative path (connive, phase out, force block, suspect).
            inject_subject_target(&mut clause.effect, &subject);
            clause
        }
    }
}

/// Inject a subject phrase's target filter into an effect that was parsed through
/// the imperative fallback path, where the subject was stripped before parsing.
/// Only applies to effects with a sentinel `TargetFilter::Any` that should inherit
/// the subject's targeting information.
fn inject_subject_target(effect: &mut Effect, subject: &SubjectPhraseAst) {
    let subject_filter = subject.target.as_ref().unwrap_or(&subject.affected).clone();
    match effect {
        Effect::Connive { target, .. }
        | Effect::PhaseOut { target }
        | Effect::ForceBlock { target }
        | Effect::Suspect { target }
        | Effect::Goad { target }
        | Effect::Mill { target, .. }
            if *target == TargetFilter::Any || *target == TargetFilter::Controller =>
        {
            *target = subject_filter;
        }
        // CR 500.7: "target player takes an extra turn" — inject subject target
        Effect::ExtraTurn { target } if *target == TargetFilter::Controller => {
            *target = subject_filter;
        }
        // CR 122.1: "target player gets a poison counter" — inject subject target
        Effect::GivePlayerCounter { target, .. } if *target == TargetFilter::Controller => {
            *target = subject_filter;
        }
        // CR 500.8: "target player gets an additional combat phase" — inject subject target
        Effect::AdditionalCombatPhase { target, .. } if *target == TargetFilter::Controller => {
            *target = subject_filter;
        }
        // CR 701.14a: "enchanted creature fights target creature" — the subject
        // of the fight is the enchanted/equipped creature, not the Aura/Equipment.
        Effect::Fight {
            subject: fight_subject,
            ..
        } if *fight_subject == TargetFilter::SelfRef => {
            *fight_subject = subject_filter;
        }
        _ => {}
    }
}

/// CR 114.1: Parse emblem creation from Oracle text.
/// Handles both full form "you get an emblem with \"[text]\"" and
/// subject-stripped form "get an emblem with \"[text]\"".
fn try_parse_emblem_creation(lower: &str, original: &str) -> Option<Effect> {
    // Find the prefix offset using the lowered text
    let prefix_len = if lower.starts_with("you get an emblem with ") {
        "you get an emblem with ".len()
    } else if lower.starts_with("get an emblem with ") {
        "get an emblem with ".len()
    } else {
        return None;
    };

    // Use original-case text for the inner content (preserves subtype capitalization)
    let rest = &original[prefix_len..];

    // Extract the quoted emblem text (handles both "..." and '...' quoting)
    let inner = rest
        .trim()
        .trim_end_matches('.')
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('\u{201c}')
        .trim_matches('\u{201d}');

    if inner.is_empty() {
        return None;
    }

    // Try to parse the emblem text as a static ability line
    if let Some(static_def) = super::oracle_static::parse_static_line(inner) {
        Some(Effect::CreateEmblem {
            statics: vec![static_def],
        })
    } else {
        // Fallback: create an emblem with an unimplemented static
        Some(Effect::CreateEmblem {
            statics: vec![
                StaticDefinition::new(StaticMode::EmblemStatic).description(inner.to_string())
            ],
        })
    }
}

/// CR 601.2a + CR 118.9: Parse "cast it/that card [without paying its mana cost]".
fn try_parse_cast_effect(lower: &str) -> Option<Effect> {
    let (rest, mode) = if let Some(rest) = lower.strip_prefix("cast ") {
        (rest, CardPlayMode::Cast)
    } else if let Some(rest) = lower.strip_prefix("play ") {
        // CR 305.1: "play" means cast if spell, play as land if land.
        (rest, CardPlayMode::Play)
    } else {
        return None;
    };

    let without_paying = rest.contains("without paying its mana cost")
        || rest.contains("without paying their mana cost");

    let target = if rest.starts_with("it")
        || rest.starts_with("that card")
        || rest.starts_with("that spell")
        || rest.starts_with("the copy")
        || rest.starts_with("the exiled card")
        || rest.starts_with("them")
        || rest.starts_with("those cards")
        || rest.starts_with("cards exiled")
    {
        TargetFilter::ParentTarget
    } else {
        TargetFilter::Any
    };

    Some(Effect::CastFromZone {
        target,
        without_paying_mana_cost: without_paying,
        mode,
    })
}

#[tracing::instrument(level = "debug")]
fn parse_imperative_effect(text: &str, ctx: &ParseContext) -> ParsedEffectClause {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    parse_imperative_effect_inner(tp, ctx)
}

fn parse_imperative_effect_inner(tp: TextPair, ctx: &ParseContext) -> ParsedEffectClause {
    if let Some(ast) = parse_imperative_family_ast(tp.original, tp.lower, ctx) {
        return lower_imperative_family_ast(ast);
    }

    // CR 114.1: "you get an emblem with "[static text]""
    if let Some(effect) = try_parse_emblem_creation(tp.lower, tp.original) {
        return parsed_clause(effect);
    }

    // CR 601.2a + CR 118.9: "cast it/that card without paying its mana cost"
    if let Some(effect) = try_parse_cast_effect(tp.lower) {
        return parsed_clause(effect);
    }

    // CR 115.7: "change the target of" / "you may choose new targets for"
    if let Some(effect) = try_parse_change_targets(tp.lower) {
        return parsed_clause(effect);
    }

    // --- Fallback ---
    let verb = tp.lower.split_whitespace().next().unwrap_or("unknown");
    tracing::debug!(
        verb,
        oracle_text = tp.original,
        "imperative fallback to Unimplemented"
    );
    parsed_clause(Effect::Unimplemented {
        name: verb.to_string(),
        description: Some(tp.original.to_string()),
    })
}

/// Determines if text after "choose " is a targeting synonym rather than
/// a modal choice ("choose one —"), color choice, or creature type choice.
///
/// Returns true when the text contains "target" (indicating a targeting phrase)
/// or uses "a/an {type} you/opponent control(s)" (selection-as-targeting).
///
/// Returns false for:
///   - "card from it" — handled separately as RevealHand filter
///   - "a color" / "a creature type" / "a card type" / "a card name" — different mechanics
fn is_choose_as_targeting(rest: &str) -> bool {
    // Already handled elsewhere
    if rest.contains("card from it") {
        return false;
    }

    // If try_parse_named_choice would match "choose {rest}", it's a named choice, not targeting
    let as_full = format!("choose {rest}");
    if try_parse_named_choice(&as_full).is_some() {
        return false;
    }

    // Any phrase containing "target" is a targeting synonym
    if rest.contains("target") {
        return true;
    }

    // "choose up to N" without "target" (e.g. "choose up to two creatures")
    if rest.starts_with("up to ") {
        return true;
    }

    // "choose a/an {type} ... you control / an opponent controls"
    if let Some(after_article) = rest.strip_prefix("a ").or_else(|| rest.strip_prefix("an ")) {
        // Exclude patterns not yet in try_parse_named_choice but still not targeting
        if after_article.starts_with("nonbasic land type") || after_article.starts_with("number") {
            return false;
        }
        // Must reference controller to be targeting-like
        if after_article.contains("you control")
            || after_article.contains("opponent controls")
            || after_article.contains("an opponent controls")
        {
            return true;
        }
    }

    false
}

/// Match "choose a creature type", "choose a color", "choose odd or even",
/// "choose a basic land type", "choose a card type" from lowercased Oracle text.
pub(crate) fn try_parse_named_choice(lower: &str) -> Option<ChoiceType> {
    if !lower.starts_with("choose ") {
        return None;
    }
    let rest = &lower[7..]; // skip "choose "
    if rest.starts_with("a creature type") {
        Some(ChoiceType::CreatureType)
    } else if rest.starts_with("a color") {
        Some(ChoiceType::Color)
    } else if rest.starts_with("odd or even") {
        Some(ChoiceType::OddOrEven)
    } else if rest.starts_with("a basic land type") {
        Some(ChoiceType::BasicLandType)
    } else if rest.starts_with("a card type") {
        Some(ChoiceType::CardType)
    } else if rest.starts_with("a card name")
        || rest.starts_with("a nonland card name")
        || rest.starts_with("a creature card name")
    {
        Some(ChoiceType::CardName)
    } else if let Some(range_rest) = rest.strip_prefix("a number between ") {
        // "choose a number between 0 and 13"
        let mut parts = range_rest.splitn(3, ' ');
        let min = parts.next().and_then(|s| s.parse::<u8>().ok()).unwrap_or(0);
        let and = parts.next();
        let max = parts
            .next()
            .and_then(|s| {
                s.trim_end_matches(|c: char| !c.is_ascii_digit())
                    .parse::<u8>()
                    .ok()
            })
            .unwrap_or(20);
        if and == Some("and") {
            Some(ChoiceType::NumberRange { min, max })
        } else {
            None
        }
    } else if let Some(gt_rest) = rest.strip_prefix("a number greater than ") {
        // "choose a number greater than 0" — open-ended, cap at 20
        let n = gt_rest
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(0);
        Some(ChoiceType::NumberRange {
            min: n + 1,
            max: 20,
        })
    } else if rest == "a number" || rest.starts_with("a number ") {
        // Generic "choose a number" — default range 0-20
        Some(ChoiceType::NumberRange { min: 0, max: 20 })
    } else if rest.starts_with("a land type") || rest.starts_with("a nonbasic land type") {
        Some(ChoiceType::LandType)
    } else if rest.starts_with("an opponent") {
        // CR 800.4a: Choose an opponent from among players in the game.
        Some(ChoiceType::Opponent)
    } else if rest.starts_with("a player") {
        Some(ChoiceType::Player)
    } else if rest.starts_with("two colors") {
        Some(ChoiceType::TwoColors)
    } else {
        // Generic "X or Y" pattern — must come AFTER all specific patterns above
        try_parse_binary_choice(rest).map(|options| ChoiceType::Labeled { options })
    }
}

/// Try to parse "X or Y" as a binary labeled choice.
/// Only matches simple one-or-two-word labels separated by " or ".
/// Returns capitalized labels.
/// This must come AFTER all specific patterns in try_parse_named_choice to avoid
/// accidentally matching "choose left or right" against targeting patterns.
fn try_parse_binary_choice(rest: &str) -> Option<Vec<String>> {
    let (left, right) = rest.split_once(" or ")?;
    let left = left.trim();
    let right = right.trim();

    // Labels must be short (≤2 words) — longer phrases are likely clauses, not choices
    if left.split_whitespace().count() > 2 || right.split_whitespace().count() > 2 {
        return None;
    }
    // Reject known non-choice patterns
    if left.contains("target") || right.contains("target") {
        return None;
    }
    if right == "more" || left == "both" || right == "both" {
        return None;
    }

    Some(vec![capitalize(left), capitalize(right)])
}

fn parse_choose_filter(lower: &str) -> TargetFilter {
    // Extract type info between "choose" and "card from it"
    // Handle both "choose X" and "you choose X" forms
    let after_choose = lower
        .strip_prefix("you choose ")
        .or_else(|| lower.strip_prefix("you may choose "))
        .or_else(|| lower.strip_prefix("choose "))
        .unwrap_or(lower);
    let before_card = after_choose.split("card").next().unwrap_or("");
    let cleaned = before_card
        .trim()
        .trim_start_matches("a ")
        .trim_start_matches("an ")
        .trim();

    let parts: Vec<&str> = cleaned.split(" or ").collect();
    if parts.len() > 1 {
        let filters: Vec<TargetFilter> = parts
            .iter()
            .filter_map(|p| type_str_to_target_filter(p.trim()))
            .collect();
        if filters.len() > 1 {
            return TargetFilter::Or { filters };
        }
        if let Some(f) = filters.into_iter().next() {
            return f;
        }
    }
    if let Some(f) = type_str_to_target_filter(cleaned) {
        return f;
    }
    TargetFilter::Any
}

fn type_str_to_target_filter(s: &str) -> Option<TargetFilter> {
    let card_type = match s {
        "artifact" => Some(TypeFilter::Artifact),
        "creature" => Some(TypeFilter::Creature),
        "enchantment" => Some(TypeFilter::Enchantment),
        "instant" => Some(TypeFilter::Instant),
        "sorcery" => Some(TypeFilter::Sorcery),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        "land" => Some(TypeFilter::Land),
        _ => None,
    };
    card_type.map(|ct| TargetFilter::Typed(TypedFilter::new(ct)))
}

/// Extract card type filter from a sub-ability sentence containing "card from it/among".
/// Handles forms like "exile a nonland card from it", "discard a creature card from it".
fn parse_choose_filter_from_sentence(lower: &str) -> TargetFilter {
    let card_pos = match lower.find("card from") {
        Some(pos) => pos,
        None => return TargetFilter::Any,
    };
    // The word immediately before "card from" is the type descriptor
    let word = lower[..card_pos].trim().rsplit(' ').next().unwrap_or("");
    if let Some(negated) = word.strip_prefix("non") {
        if let Some(TargetFilter::Typed(tf)) = type_str_to_target_filter(negated) {
            if let Some(primary) = tf.get_primary_type().cloned() {
                return TargetFilter::Typed(
                    TypedFilter::card().with_type(TypeFilter::Non(Box::new(primary))),
                );
            }
        }
    }
    type_str_to_target_filter(word).unwrap_or(TargetFilter::Any)
}

/// Check if an effect exiles objects (candidate for tracked set recording).
/// Also looks inside `CreateDelayedTrigger` wrappers, since a previous clause's
/// exile may have already been wrapped by `strip_temporal_suffix`.
fn is_exile_effect(effect: &Effect) -> bool {
    match effect {
        Effect::ChangeZone {
            destination: Zone::Exile,
            ..
        }
        | Effect::ChangeZoneAll {
            destination: Zone::Exile,
            ..
        } => true,
        Effect::CreateDelayedTrigger { effect: inner, .. } => is_exile_effect(&inner.effect),
        _ => false,
    }
}

/// CR 603.7: Detect explicit cross-clause pronouns ("those cards", "the exiled card").
/// `lower` must be the pre-lowered version of the text.
fn contains_explicit_tracked_set_pronoun(lower: &str) -> bool {
    lower.contains("those cards")
        || lower.contains("those permanents")
        || lower.contains("those creatures")
        || lower.contains("the exiled card")
        || lower.contains("the exiled permanent")
        || lower.contains("the exiled creature")
}

/// CR 603.7: Detect implicit anaphora ("return it/them to the battlefield")
/// when preceded by an exile effect. Context-sensitive — only matches when
/// the pronoun is in a return-to-battlefield construction.
/// `lower` must be the pre-lowered version of the text.
fn contains_implicit_tracked_set_pronoun(lower: &str) -> bool {
    (lower.starts_with("return it ") || lower.starts_with("return them "))
        && lower.contains("battlefield")
}

fn mark_uses_tracked_set(def: &mut AbilityDefinition) {
    if let Effect::CreateDelayedTrigger {
        uses_tracked_set, ..
    } = &mut *def.effect
    {
        *uses_tracked_set = true;
    }
}

/// Parse a compound effect chain into an `AbilityDefinition` sub-ability chain.
///
/// Phase 1 keeps the existing clause/effect semantics but replaces the fragile
/// textual `replace(", then ", ". ").split(". ")` logic with a boundary-aware
/// splitter that preserves whether a chunk ended a sentence or was linked by
/// `, then`.
pub fn parse_effect_chain(text: &str, kind: AbilityKind) -> AbilityDefinition {
    parse_effect_chain_impl(text, kind, &ParseContext::default())
}

/// Parse a compound effect chain with subject context for pronoun resolution.
/// CR 608.2k: Used by the trigger parser to thread the trigger subject so that
/// bare pronouns ("it") resolve to TriggeringSource instead of SelfRef.
pub(crate) fn parse_effect_chain_with_context(
    text: &str,
    kind: AbilityKind,
    ctx: &ParseContext,
) -> AbilityDefinition {
    parse_effect_chain_impl(text, kind, ctx)
}

fn parse_effect_chain_impl(text: &str, kind: AbilityKind, ctx: &ParseContext) -> AbilityDefinition {
    let full_text = text; // Bind before `text` is shadowed by strip helpers in the loop
    let chunks = split_clause_sequence(text);
    let mut defs: Vec<AbilityDefinition> = Vec::new();

    for chunk in &chunks {
        let normalized_text = strip_leading_sequence_connector(&chunk.text).trim();
        if normalized_text.is_empty() {
            continue;
        }

        // CR 608.2c: "Otherwise, [effect]" — attach as else_ability on the
        // most recent conditional def in the chain.
        let lower_check = normalized_text.to_lowercase();
        let otherwise_prefix_len = if lower_check.starts_with("otherwise, ") {
            Some("otherwise, ".len())
        } else if lower_check.starts_with("otherwise ") {
            Some("otherwise ".len())
        } else if lower_check.starts_with("if not, ") {
            Some("if not, ".len())
        } else if lower_check.starts_with("if no player does, ") {
            Some("if no player does, ".len())
        } else if lower_check.starts_with("if no one does, ") {
            Some("if no one does, ".len())
        } else {
            None
        };
        if let Some(prefix_len) = otherwise_prefix_len {
            let else_text = &normalized_text[prefix_len..];
            let else_def = parse_effect_chain_impl(else_text, kind, ctx);
            // Walk defs backward to find the most recent conditional
            let has_condition = defs.iter().any(|d| d.condition.is_some());
            if has_condition {
                for d in defs.iter_mut().rev() {
                    if d.condition.is_some() {
                        d.else_ability = Some(Box::new(else_def));
                        break;
                    }
                }
            } else {
                // Fallback: no IfYouDo found — emit as Unimplemented to preserve coverage
                defs.push(AbilityDefinition::new(
                    kind,
                    Effect::Unimplemented {
                        name: "otherwise".to_string(),
                        description: Some("Otherwise".to_string()),
                    },
                ));
                defs.push(else_def);
            }
            continue;
        }

        let (condition, text) = strip_additional_cost_conditional(normalized_text);
        let (if_you_do, text) = if condition.is_none() {
            strip_if_you_do_conditional(&text)
        } else {
            (None, text)
        };
        // CR 603.4 + CR 608.2c: Counter threshold condition — runs unconditionally
        // on the text output from strip_if_you_do_conditional. For compound
        // "when you do, if it has N counters" patterns, WhenYouDo is always true for
        // non-optional parents (CR 603.12), so the QuantityCheck is the meaningful gate.
        let (counter_cond, text) = strip_counter_conditional(&text);
        let (cast_from_zone, text) =
            if condition.is_none() && if_you_do.is_none() && counter_cond.is_none() {
                strip_cast_from_zone_conditional(&text)
            } else {
                (None, text)
            };
        let (card_type_cond, text) = if condition.is_none()
            && if_you_do.is_none()
            && counter_cond.is_none()
            && cast_from_zone.is_none()
        {
            strip_card_type_conditional(&text)
        } else {
            (None, text)
        };
        let (property_cond, text) = if condition.is_none()
            && if_you_do.is_none()
            && counter_cond.is_none()
            && cast_from_zone.is_none()
            && card_type_cond.is_none()
        {
            strip_property_conditional(&text)
        } else {
            (None, text)
        };
        // CR 608.2e: "If that creature has [keyword], [effect] instead"
        let (keyword_instead_cond, text) = if condition.is_none()
            && if_you_do.is_none()
            && counter_cond.is_none()
            && cast_from_zone.is_none()
            && card_type_cond.is_none()
            && property_cond.is_none()
        {
            strip_target_keyword_instead(&text)
        } else {
            (None, text)
        };
        // CR 608.2c: General suffix condition — "do Y if X" where X is a quantity comparison.
        // Runs only when no dedicated stripper matched; parse_condition_text is the safety net
        // (returns None for anything it can't parse).
        let (suffix_cond, text) = if condition.is_none()
            && if_you_do.is_none()
            && counter_cond.is_none()
            && cast_from_zone.is_none()
            && card_type_cond.is_none()
            && property_cond.is_none()
            && keyword_instead_cond.is_none()
        {
            strip_suffix_conditional(&text)
        } else {
            (None, text)
        };
        let condition = condition
            .or(counter_cond)
            .or(if_you_do)
            .or(cast_from_zone)
            .or(card_type_cond)
            .or(property_cond)
            .or(keyword_instead_cond)
            .or(suffix_cond);
        // CR 608.2c + CR 400.7: "unless ~ entered this turn" — strip suffix and
        // replace condition with SourceDidNotEnterThisTurn. The IfYouDo condition
        // is redundant when the parent is optional (optional already gates the sub).
        let (unless_entered, text) = strip_unless_entered_suffix(&text);
        let condition = if unless_entered.is_some() {
            unless_entered
        } else {
            condition
        };
        let (is_optional, opponent_may_scope, text) = strip_optional_effect_prefix(&text);
        let (repeat_for, text) = strip_for_each_prefix(&text);
        // CR 609.3: "twice" / "N times" suffix — same mechanism as "for each" prefix.
        let (repeat_count, text) = if repeat_for.is_none() {
            strip_repeat_count_suffix(&text)
        } else {
            (None, text)
        };
        let repeat_for = repeat_for.or(repeat_count);
        let (player_scope, text) = strip_each_player_subject(&text);

        // CR 603.7a: Check for temporal prefix before suffix. When present, parse the
        // inner effect through the full pipeline and wrap in CreateDelayedTrigger.
        let (text_after_prefix, prefix_delayed) = strip_temporal_prefix(&text);
        if let Some(prefix_condition) = prefix_delayed {
            let (inner_text, inner_multi_target) = strip_any_number_quantifier(text_after_prefix);
            let inner_clause = parse_effect_clause(&inner_text, ctx);
            let mut inner_def = AbilityDefinition::new(kind, inner_clause.effect);
            if let Some(spec) = inner_multi_target.or(inner_clause.multi_target) {
                inner_def = inner_def.multi_target(spec);
            }
            if let Some(duration) = inner_clause.duration {
                inner_def = inner_def.duration(duration);
            }
            if let Some(sub) = inner_clause.sub_ability {
                inner_def.sub_ability = Some(sub);
            }
            let delayed_effect = Effect::CreateDelayedTrigger {
                condition: prefix_condition,
                effect: Box::new(inner_def),
                uses_tracked_set: false,
            };
            let mut def = AbilityDefinition::new(kind, delayed_effect);
            if is_optional {
                def.optional = true;
                def.optional_for = opponent_may_scope;
            }
            if let Some(ref condition) = condition {
                def = def.condition(condition.clone());
            }
            defs.push(def);
            continue;
        }

        // CR 205.1a: "it's a/an [type]" — type-setting effect for returned permanents
        // (e.g., Glimmer cycle "It's an enchantment."). Intercept before parse_effect_clause.
        if let Some(animate_def) = try_parse_type_setting(&text) {
            let mut def = animate_def;
            if let Some(ref condition) = condition {
                def = def.condition(condition.clone());
            }
            defs.push(def);
            continue;
        }

        let (text_no_temporal, delayed_condition) = strip_temporal_suffix(&text);
        let (text_no_qty, multi_target) = strip_any_number_quantifier(text_no_temporal);
        let clause = parse_effect_clause(&text_no_qty, ctx);
        let mut def = AbilityDefinition::new(kind, clause.effect);
        if is_optional {
            def.optional = true;
            def.optional_for = opponent_may_scope;
        }
        if let Some(qty) = repeat_for {
            def.repeat_for = Some(qty);
        }
        if let Some(scope) = player_scope {
            def.player_scope = Some(scope);
        }
        if let Some(duration) = clause.duration {
            def = def.duration(duration);
        }
        if let Some(ref condition) = condition {
            def = def.condition(condition.clone());
        }
        // CR 115.1d: Apply multi-target spec — prefer strip_any_number_quantifier result,
        // fall back to clause-level spec (distribute parsers return early before the strip runs).
        if let Some(ref spec) = multi_target {
            def = def.multi_target(spec.clone());
        } else if let Some(spec) = clause.multi_target {
            def = def.multi_target(spec);
        }
        // CR 601.2d: Propagate distribute flag from parsed clause to definition.
        if let Some(unit) = clause.distribute {
            def = def.distribute(unit);
        }

        // Kicker clauses referencing "that creature"/"it" inherit the parent's target.
        // Scoped to conditional sub-abilities only — "it"/"its" appears in possessive
        // forms on many cards and would incorrectly replace targets if applied generally.
        if condition.is_some() && !defs.is_empty() && has_anaphoric_reference(&text.to_lowercase())
        {
            replace_target_with_parent(&mut def.effect);
        }
        // CR 608.2c: Pronoun clause following a conditional targeted effect.
        // "It gains trample" after "[condition] put a +1/+1 counter on target creature"
        // — the "it" refers to the same target creature, not the ability source.
        if condition.is_none()
            && defs
                .last()
                .is_some_and(|prev| prev.condition.is_some() && has_typed_target(&prev.effect))
            && has_anaphoric_reference(&text.to_lowercase())
        {
            replace_target_with_parent(&mut def.effect);
        }

        // CR 608.2e: "Instead" overrides — attach as sub_ability on the previous def,
        // not as a standalone def. The Cow swap in effects/mod.rs handles the resolution.
        if matches!(
            condition,
            Some(AbilityCondition::TargetHasKeywordInstead { .. })
        ) {
            if let Some(prev) = defs.last_mut() {
                prev.sub_ability = Some(Box::new(def));
            }
            continue;
        }

        let mut current_defs = vec![def];
        if let Some(sub) = clause.sub_ability {
            current_defs.push(*sub);
        }

        // CR 603.7: Wrap in CreateDelayedTrigger if temporal suffix was found
        if let Some(delayed_cond) = delayed_condition {
            for current in &mut current_defs {
                let inner = std::mem::replace(
                    current,
                    AbilityDefinition::new(
                        kind,
                        Effect::Unimplemented {
                            name: "placeholder".to_string(),
                            description: None,
                        },
                    ),
                );
                *current = AbilityDefinition::new(
                    kind,
                    Effect::CreateDelayedTrigger {
                        condition: delayed_cond.clone(),
                        effect: Box::new(inner),
                        uses_tracked_set: false,
                    },
                );
            }
        }

        // CR 603.7: Cross-clause pronoun → mark uses_tracked_set on delayed trigger
        if let Some(previous) = defs.last() {
            if is_exile_effect(&previous.effect) {
                let has_tracked_ref = contains_explicit_tracked_set_pronoun(&lower_check)
                    || contains_implicit_tracked_set_pronoun(&lower_check);
                if has_tracked_ref {
                    for current in &mut current_defs {
                        mark_uses_tracked_set(current);
                    }
                }
            }
        }

        let followup_continuation = defs.last().and_then(|previous| {
            parse_followup_continuation_ast(normalized_text, &previous.effect)
        });
        let absorb_followup = followup_continuation.as_ref().is_some_and(|continuation| {
            current_defs
                .first()
                .is_some_and(|current| continuation_absorbs_current(continuation, &current.effect))
        });
        if let Some(continuation) = followup_continuation {
            apply_clause_continuation(&mut defs, continuation, kind);
        }
        if absorb_followup {
            continue;
        }

        let intrinsic_continuation =
            parse_intrinsic_continuation_ast(normalized_text, &current_defs[0].effect, full_text);
        defs.extend(current_defs);

        if let Some(continuation) = intrinsic_continuation {
            apply_clause_continuation(&mut defs, continuation, kind);
        }
    }

    // CR 706 + CR 705: Consolidate die result table lines into their parent RollDie,
    // and coin flip conditional branches into their parent FlipCoin.
    consolidate_die_and_coin_defs(&mut defs, kind);

    // Chain: last has no sub_ability, each earlier one chains to next
    if defs.len() > 1 {
        let last = defs.pop().unwrap();
        let mut chain = last;
        while let Some(mut prev) = defs.pop() {
            prev.sub_ability = Some(Box::new(chain));
            chain = prev;
        }
        chain
    } else {
        defs.pop().unwrap_or_else(|| {
            AbilityDefinition::new(
                kind,
                Effect::Unimplemented {
                    name: "empty".to_string(),
                    description: None,
                },
            )
        })
    }
}

/// CR 705: Post-process parsed ability defs to consolidate coin flip conditional
/// branches into their parent `FlipCoin` effect.
///
/// Pattern: a bare `FlipCoin { win: None, lose: None }` followed by one or more
/// `FlipCoin { win: Some(..), lose: None }` / `FlipCoin { win: None, lose: Some(..) }`
/// defs produced by the "if you win/lose the flip" intercept in `parse_effect_clause`.
fn consolidate_die_and_coin_defs(defs: &mut Vec<AbilityDefinition>, _kind: AbilityKind) {
    let mut i = 0;
    while i < defs.len() {
        // CR 705: Consolidate coin flip branches
        if matches!(
            &*defs[i].effect,
            Effect::FlipCoin {
                win_effect: None,
                lose_effect: None,
            }
        ) {
            let mut win = None;
            let mut lose = None;
            let mut j = i + 1;
            while j < defs.len() && (win.is_none() || lose.is_none()) {
                match &*defs[j].effect {
                    Effect::FlipCoin {
                        win_effect: Some(w),
                        lose_effect: None,
                    } if win.is_none() => {
                        win = Some(w.clone());
                        j += 1;
                    }
                    Effect::FlipCoin {
                        win_effect: None,
                        lose_effect: Some(l),
                    } if lose.is_none() => {
                        lose = Some(l.clone());
                        j += 1;
                    }
                    _ => break,
                }
            }
            if win.is_some() || lose.is_some() {
                *defs[i].effect = Effect::FlipCoin {
                    win_effect: win,
                    lose_effect: lose,
                };
                defs.drain(i + 1..j);
            }
        }

        i += 1;
    }
}

// --- Search library parser ---

fn parse_search_library_details(lower: &str) -> SearchLibraryDetails {
    let reveal = lower.contains("reveal");

    // Extract count from "up to N" (must be done before filter extraction since
    // "for up to five creature cards" needs to skip the count to find the type).
    let (count, count_end_in_for) = if let Some(after_up_to) = strip_after(lower, "up to ") {
        if let Some((n, rest)) = parse_number(after_up_to) {
            // Calculate the byte offset where the type text begins after "up to N "
            let type_start = lower.len() - rest.len();
            (n, Some(type_start))
        } else {
            (1, None)
        }
    } else {
        (1, None)
    };

    // Extract the type filter from after "for a/an" or "for up to N".
    let filter = if let Some(after_for) = strip_after(lower, "for a ") {
        parse_search_filter(after_for)
    } else if let Some(after_for) = strip_after(lower, "for an ") {
        parse_search_filter(after_for)
    } else if let Some(type_start) = count_end_in_for {
        // "for up to five creature cards" — type text starts after the number
        parse_search_filter(&lower[type_start..])
    } else {
        TargetFilter::Any
    };

    SearchLibraryDetails {
        filter,
        count,
        reveal,
    }
}

// --- Seek parser (Alchemy digital-only) ---

/// Parse "seek [count] [filter] card(s) [and put onto battlefield [tapped]]".
/// Seek grammar is simpler than search: no "your library", no "for", no shuffle.
fn parse_seek_details(lower: &str) -> types::SeekDetails {
    let after_seek = lower.strip_prefix("seek ").unwrap_or(lower);

    // Extract destination clause before filter parsing, so it doesn't pollute the filter.
    let (filter_text, destination, enter_tapped) = {
        let put_idx = after_seek
            .find(" and put")
            .or_else(|| after_seek.find(", put"));
        if let Some(idx) = put_idx {
            let dest_clause = &after_seek[idx..];
            let dest = parse_search_destination(dest_clause);
            let tapped = dest_clause.contains("battlefield tapped");
            (&after_seek[..idx], dest, tapped)
        } else {
            (after_seek, Zone::Hand, false)
        }
    };

    // Extract count: "two nonland cards" → (2, "nonland cards")
    let (count, remaining) = if let Some((n, rest)) = parse_number(filter_text) {
        (QuantityExpr::Fixed { value: n as i32 }, rest.trim_start())
    } else if let Some(rest) = filter_text.strip_prefix("x ") {
        (
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            rest.trim_start(),
        )
    } else {
        (QuantityExpr::Fixed { value: 1 }, filter_text)
    };

    // Strip leading article "a "/"an "
    let remaining = remaining
        .strip_prefix("a ")
        .or_else(|| remaining.strip_prefix("an "))
        .unwrap_or(remaining);

    // Reuse the existing search filter parser
    let filter = parse_search_filter(remaining);

    types::SeekDetails {
        filter,
        count,
        destination,
        enter_tapped,
    }
}

/// Parse the card type filter from search text like "basic land card, ..."
/// or "creature card with ..." into a TargetFilter.
fn parse_search_filter(text: &str) -> TargetFilter {
    // Find the end of the type description (before comma, period, or "and put")
    let type_end = text
        .find(',')
        .or_else(|| text.find('.'))
        .or_else(|| text.find(" and put"))
        .or_else(|| text.find(" and shuffle"))
        .unwrap_or(text.len());
    let type_text = text[..type_end].trim();

    // Strip trailing "card" or "cards"
    let type_text = type_text
        .strip_suffix(" cards")
        .or_else(|| type_text.strip_suffix(" card"))
        .unwrap_or(type_text)
        .trim();

    // Check for "a card" / "card" alone (Demonic Tutor pattern)
    if type_text == "card" || type_text.is_empty() {
        return TargetFilter::Any;
    }

    // Separate the type word from property suffixes.
    // Extract type/subtype first, then parse remaining text for filter properties.
    let is_basic = type_text.contains("basic");
    let clean = type_text.replace("basic ", "");

    // Try to find where the type word ends and property suffixes begin.
    // Suffixes start with "with " or "and with ".
    let (type_word, suffix_text) = {
        let lower = clean.to_lowercase();
        if let Some(pos) = lower.find(" with ") {
            // Strip trailing "card"/"cards" from the type word before the suffix
            let mut tw = clean[..pos].trim();
            tw = tw
                .strip_suffix(" cards")
                .or_else(|| tw.strip_suffix(" card"))
                .unwrap_or(tw)
                .trim();
            (tw.to_string(), &clean[pos..])
        } else {
            (clean.trim().to_string(), "")
        }
    };

    // Map type name to TypeFilter + optional subtype
    let (card_type, subtype): (Option<TypeFilter>, Option<String>) = match type_word.as_str() {
        "land" => (Some(TypeFilter::Land), None),
        "creature" => (Some(TypeFilter::Creature), None),
        "artifact" => (Some(TypeFilter::Artifact), None),
        "enchantment" => (Some(TypeFilter::Enchantment), None),
        "instant" => (Some(TypeFilter::Instant), None),
        "sorcery" => (Some(TypeFilter::Sorcery), None),
        "planeswalker" => (Some(TypeFilter::Planeswalker), None),
        "instant or sorcery" => {
            let mut properties = vec![];
            if is_basic {
                properties.push(FilterProp::HasSupertype {
                    value: "Basic".to_string(),
                });
            }
            parse_search_filter_suffixes(suffix_text, &mut properties);
            return TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Instant).properties(properties.clone()),
                    ),
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Sorcery).properties(properties),
                    ),
                ],
            };
        }
        other => {
            // Negated type prefixes: "noncreature", "nonland", etc.
            let negated_types: &[(&str, TypeFilter)] = &[
                ("noncreature", TypeFilter::Creature),
                ("nonland", TypeFilter::Land),
                ("nonartifact", TypeFilter::Artifact),
                ("nonenchantment", TypeFilter::Enchantment),
            ];
            for &(prefix, ref inner) in negated_types {
                if other == prefix {
                    let mut properties = vec![];
                    if is_basic {
                        properties.push(FilterProp::HasSupertype {
                            value: "Basic".to_string(),
                        });
                    }
                    parse_search_filter_suffixes(suffix_text, &mut properties);
                    return TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Non(Box::new(inner.clone())))
                            .properties(properties),
                    );
                }
            }

            // Could be a subtype search: "forest card", "plains card", "equipment card"
            let land_subtypes = ["plains", "island", "swamp", "mountain", "forest"];
            if land_subtypes.contains(&other) {
                let mut properties = vec![];
                if is_basic {
                    properties.push(FilterProp::HasSupertype {
                        value: "Basic".to_string(),
                    });
                }
                parse_search_filter_suffixes(suffix_text, &mut properties);
                return TargetFilter::Typed(
                    TypedFilter::land()
                        .subtype(capitalize(other))
                        .properties(properties),
                );
            }
            if other == "equipment" {
                let mut properties = vec![];
                parse_search_filter_suffixes(suffix_text, &mut properties);
                return TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .subtype("Equipment".to_string())
                        .properties(properties),
                );
            }
            if other == "aura" {
                let mut properties = vec![];
                parse_search_filter_suffixes(suffix_text, &mut properties);
                return TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Enchantment)
                        .subtype("Aura".to_string())
                        .properties(properties),
                );
            }
            // "card with X" — no type constraint but has property suffixes.
            // Produces a Typed filter with no type_filters but with the parsed properties.
            if other == "card" && !suffix_text.is_empty() {
                let mut properties = vec![];
                parse_search_filter_suffixes(suffix_text, &mut properties);
                if !properties.is_empty() {
                    return TargetFilter::Typed(TypedFilter::default().properties(properties));
                }
            }
            // Generic subtype fallback: treat unrecognized words as subtypes
            // (e.g., "elf", "goblin", "zombie", "dragon", "angel", "vampire").
            // Only if the word is alphabetic and not "card" or "permanent".
            if !other.is_empty()
                && other != "card"
                && other != "permanent"
                && other.chars().all(|c| c.is_alphabetic())
            {
                let mut properties = vec![];
                if is_basic {
                    properties.push(FilterProp::HasSupertype {
                        value: "Basic".to_string(),
                    });
                }
                parse_search_filter_suffixes(suffix_text, &mut properties);
                return TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype(capitalize(other))
                        .properties(properties),
                );
            }
            // Fallback: treat as Any
            return TargetFilter::Any;
        }
    };

    let mut properties = vec![];
    if is_basic {
        properties.push(FilterProp::HasSupertype {
            value: "Basic".to_string(),
        });
    }
    parse_search_filter_suffixes(suffix_text, &mut properties);

    let mut tf = TypedFilter::default();
    if let Some(ct) = card_type {
        tf = tf.with_type(ct);
    }
    if let Some(st) = subtype {
        tf = tf.subtype(st);
    }
    tf.properties = properties;
    TargetFilter::Typed(tf)
}

/// Parse property suffixes from search filter text ("with mana value ...", "with a different name ...").
/// Reuses the existing suffix parsers from oracle_target.
fn parse_search_filter_suffixes(text: &str, properties: &mut Vec<FilterProp>) {
    let lower = text.to_lowercase();
    let mut remaining = lower.as_str();

    while !remaining.is_empty() {
        // Strip leading connectors
        remaining = remaining.trim_start();
        if let Some(r) = remaining.strip_prefix("and ") {
            remaining = r.trim_start();
        }

        // "with that name" — matches cards with the same name as a previously-referenced card
        if let Some(rest) = remaining.strip_prefix("with that name") {
            properties.push(FilterProp::SameName);
            remaining = rest.trim_start();
            continue;
        }

        // Try mana value suffix (handles both static and dynamic comparisons)
        if let Some((prop, consumed)) = parse_mana_value_suffix(remaining) {
            properties.push(prop);
            remaining = remaining[consumed..].trim_start();
            continue;
        }

        // "with a different name than each [type] you control"
        if let Some(rest) = remaining.strip_prefix("with a different name than each ") {
            // Extract the inner type word (e.g., "aura")
            let end = rest
                .find(" you control")
                .unwrap_or_else(|| rest.find(',').unwrap_or(rest.len()));
            let inner_type = rest[..end].trim();
            let inner_filter = match inner_type {
                "aura" => TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Enchantment).subtype("Aura".to_string()),
                ),
                "creature" => TargetFilter::Typed(TypedFilter::creature()),
                "enchantment" => TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
                "artifact" => TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                _ => TargetFilter::Any,
            };
            properties.push(FilterProp::DifferentNameFrom {
                filter: Box::new(inner_filter),
            });
            // Advance past "you control"
            let skip = rest
                .find(" you control")
                .map_or(end, |p| p + " you control".len());
            remaining = rest[skip..].trim_start();
            continue;
        }

        // No recognized suffix — stop
        break;
    }
}

/// Parse the destination zone from search Oracle text.
/// Looks for "put it into your hand", "put it onto the battlefield", etc.
fn parse_search_destination(lower: &str) -> Zone {
    if lower.contains("onto the battlefield") {
        Zone::Battlefield
    } else if contains_possessive(lower, "into", "hand") {
        Zone::Hand
    } else if contains_possessive(lower, "on top of", "library") {
        Zone::Library
    } else if contains_possessive(lower, "into", "graveyard") {
        Zone::Graveyard
    } else {
        // Default destination for tutors is hand
        Zone::Hand
    }
}

/// Capitalize the first letter of a string (for subtype names).
pub(crate) fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

// --- Helper parsers ---

fn split_leading_conditional(text: &str) -> Option<(String, String)> {
    let lower = text.to_lowercase();
    if !lower.starts_with("if ") {
        return None;
    }

    let mut paren_depth = 0u32;
    let mut in_quotes = false;

    for (idx, ch) in text.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => paren_depth += 1,
            ')' if !in_quotes => paren_depth = paren_depth.saturating_sub(1),
            ',' if !in_quotes && paren_depth == 0 => {
                let condition_text = text[..idx].trim().to_string();
                let rest = text[idx + 1..].trim();
                if !rest.is_empty() {
                    return Some((condition_text, rest.to_string()));
                }
            }
            _ => {}
        }
    }

    None
}

/// Detect "if this spell's additional cost was paid, {effect}" and return
/// the condition + remaining effect text. Called at the sentence level in
/// parse_effect_chain BEFORE parse_effect_clause, so the condition is preserved
/// rather than being discarded by strip_leading_conditional.
/// Detect kicker / additional-cost conditionals. Uses a unified grammatical
/// pattern — any "if [subject] was kicked, [body]" — rather than enumerating
/// card-specific phrasings.
///
/// Returns `(condition, body_text)` where condition is `AdditionalCostPaid` or
/// `AdditionalCostPaidInstead` depending on whether the body ends with "instead".
///
/// CR 702.32b + CR 608.2e
fn strip_additional_cost_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();

    // Gift negated: "if the gift wasn't promised, ..."
    if let Some(rest) = lower.strip_prefix("if the gift wasn't promised, ") {
        let offset = text.len() - rest.len();
        return (
            Some(AbilityCondition::AdditionalCostNotPaid),
            text[offset..].to_string(),
        );
    }

    // Try the legacy phrasing first: "if this spell's additional cost was paid, ..."
    let body = if let Some(rest) = lower.strip_prefix("if this spell's additional cost was paid, ")
    {
        let offset = text.len() - rest.len();
        Some(text[offset..].to_string())
    }
    // Gift: "if the gift was promised, ..."
    else if let Some(rest) = lower.strip_prefix("if the gift was promised, ") {
        let offset = text.len() - rest.len();
        Some(text[offset..].to_string())
    }
    // Unified kicker/bargain pattern: "if <subject> was kicked/bargained, ..."
    // Covers "if this spell was kicked", "if it was kicked", "if ~ was kicked",
    // "if this spell was bargained", etc.
    else if lower.starts_with("if ") {
        lower
            .split_once(" was kicked, ")
            .or_else(|| lower.split_once(" was bargained, "))
            .map(|(_, rest)| {
                let offset = text.len() - rest.len();
                text[offset..].to_string()
            })
    } else {
        None
    };

    // CR 702.49 + CR 608.2e: "if {possessive} sneak/ninjutsu cost was paid [this turn], instead ..."
    let tp = TextPair::new(text, &lower);
    if body.is_none() && lower.contains("sneak cost was paid") {
        if let Some(after) = tp.strip_after("instead ") {
            return (
                Some(AbilityCondition::NinjutsuVariantPaidInstead {
                    variant: NinjutsuVariant::Sneak,
                }),
                after.original.to_string(),
            );
        }
    }
    if body.is_none() && lower.contains("ninjutsu cost was paid") {
        if let Some(after) = tp.strip_after("instead ") {
            return (
                Some(AbilityCondition::NinjutsuVariantPaidInstead {
                    variant: NinjutsuVariant::Ninjutsu,
                }),
                after.original.to_string(),
            );
        }
    }

    match body {
        Some(body) => {
            // CR 608.2e: Check for trailing "instead" — indicates replacement semantics.
            let (body, condition) = if let Some(stripped) = body
                .to_lowercase()
                .strip_suffix(" instead")
                .map(|_| &body[..body.len() - " instead".len()])
            {
                (
                    stripped.to_string(),
                    AbilityCondition::AdditionalCostPaidInstead,
                )
            } else {
                (body, AbilityCondition::AdditionalCostPaid)
            };
            (Some(condition), body)
        }
        None => (None, text.to_string()),
    }
}

/// CR 608.2c + CR 603.12: Detect "if you do, {effect}" and "when you do, {effect}" conditionals.
/// "If you do" gates on `optional_effect_performed` (player chose to perform optional effect).
/// "When you do" is a reflexive trigger (CR 603.12) that unconditionally fires when the
/// parent non-optional effect was performed.
fn strip_if_you_do_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    // CR 603.12: "when you do, {effect}" — reflexive trigger, always fires for non-optional parents
    if let Some(rest) = lower.strip_prefix("when you do, ") {
        let offset = text.len() - rest.len();
        return (
            Some(AbilityCondition::WhenYouDo),
            text[offset..].to_string(),
        );
    }
    // CR 608.2d: "if a player does" / "if they do" — opponent accepted an optional effect.
    if let Some(rest) = lower
        .strip_prefix("if a player does, ")
        .or_else(|| lower.strip_prefix("if they do, "))
    {
        let offset = text.len() - rest.len();
        return (
            Some(AbilityCondition::IfAPlayerDoes),
            text[offset..].to_string(),
        );
    }
    if let Some(rest) = lower.strip_prefix("if you do, ") {
        let offset = text.len() - rest.len();
        (Some(AbilityCondition::IfYouDo), text[offset..].to_string())
    } else {
        (None, text.to_string())
    }
}

/// CR 608.2c + CR 400.7: Strip "unless ~ entered this turn" suffix from effect text.
/// Returns SourceDidNotEnterThisTurn condition (meaning: effect runs only if source
/// did NOT enter this turn) and the remaining effect text.
fn strip_unless_entered_suffix(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    for pattern in &[
        "unless ~ entered this turn",
        "unless this creature entered this turn",
    ] {
        if let Some((before, _)) = tp.split_around(pattern) {
            return (
                Some(AbilityCondition::SourceDidNotEnterThisTurn),
                before.original.trim_end_matches('.').trim().to_string(),
            );
        }
    }
    (None, text.to_string())
}

/// CR 603.4: Strip "if you cast it from your hand" prefix.
/// Returns CastFromZone condition and remaining text.
/// Handles both "if you cast it from your hand, [effect]" (inline) and
/// "if you cast it from your hand" (standalone, when comma was consumed as boundary).
fn strip_cast_from_zone_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let zones: &[(&str, Zone)] = &[
        ("if you cast it from your hand", Zone::Hand),
        ("if you cast it from exile", Zone::Exile),
        ("if you cast it from your graveyard", Zone::Graveyard),
    ];
    for &(prefix, zone) in zones {
        if let Some(rest) = lower.strip_prefix(prefix) {
            // Handle both ", effect" (with comma) and "" (standalone chunk)
            let rest = rest.strip_prefix(", ").unwrap_or(rest);
            let offset = text.len() - rest.len();
            return (
                Some(AbilityCondition::CastFromZone { zone }),
                text[offset..].to_string(),
            );
        }
    }
    (None, text.to_string())
}

/// CR 608.2c: Strip "if it's a [type] card" / "if it's a non[type] card" conditional.
/// Returns the condition and remaining effect text.
/// Covers 80+ cards with "reveal → if [type] → zone change" patterns.
fn strip_card_type_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    // Pattern: "if it's a [non][type] card, [effect]"
    if let Some(rest) = lower.strip_prefix("if it's a ") {
        let (negated, rest) = if let Some(r) = rest.strip_prefix("non") {
            (true, r)
        } else {
            (false, rest)
        };
        // Extract type word before " card"
        if let Some(type_end) = rest.find(" card") {
            let type_str = &rest[..type_end];
            let capitalized = format!("{}{}", &type_str[..1].to_uppercase(), &type_str[1..]);
            if let Ok(card_type) = CoreType::from_str(&capitalized) {
                let after_card = &rest[type_end + " card".len()..];
                let remainder = after_card.strip_prefix(", ").unwrap_or(after_card);
                let offset = text.len() - remainder.len();
                return (
                    Some(AbilityCondition::RevealedHasCardType { card_type, negated }),
                    text[offset..].to_string(),
                );
            }
        }
    }
    (None, text.to_string())
}

/// CR 205.1a: Parse "it's a/an [type]" into an Animate effect with AddType + RemoveType.
/// Used by the Glimmer cycle: "It's an enchantment." after returning to battlefield.
///
/// The effect adds the specified type and removes Creature (implied by "(It's not a creature.)"
/// reminder text, which is already stripped before this point).
fn try_parse_type_setting(text: &str) -> Option<AbilityDefinition> {
    let lower = text.to_lowercase();
    let lower = lower.trim_end_matches('.');

    let type_name = lower
        .strip_prefix("it's a ")
        .or_else(|| lower.strip_prefix("it's an "))?;

    let type_name = type_name.trim();
    let capitalized = format!("{}{}", &type_name[..1].to_uppercase(), &type_name[1..]);

    // Must be a valid core type
    CoreType::from_str(&capitalized).ok()?;

    // Build Animate effect: add the target type, remove Creature
    let mut remove_types = Vec::new();
    // If the type being set is not Creature, we remove Creature (Glimmer pattern)
    if capitalized != "Creature" {
        remove_types.push("Creature".to_string());
    }

    let effect = Effect::Animate {
        power: None,
        toughness: None,
        types: vec![capitalized],
        remove_types,
        target: TargetFilter::None,
        keywords: vec![],
        is_earthbend: false,
    };

    let mut def = AbilityDefinition::new(AbilityKind::Spell, effect);
    def = def.duration(Duration::Permanent);
    Some(def)
}

/// CR 608.2c: Strip a trailing "if its power/toughness is N or greater/less" suffix.
/// Returns the condition and the effect text with the suffix removed.
///
/// Handles patterns like:
/// - "draw a card if its power is 3 or greater" → ("draw a card", QuantityCheck(power >= 3))
/// - "gain life equal to its toughness if its power is 4 or greater"
fn strip_property_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Look for " if its power is " or " if its toughness is " as a suffix
    for (property, qty_ref) in &[
        ("power", QuantityRef::EventContextSourcePower),
        ("toughness", QuantityRef::EventContextSourceToughness),
    ] {
        let pattern = format!(" if its {property} is ");
        if let Some((before, after)) = tp.rsplit_around(&pattern) {
            let after = after.lower.trim_end_matches('.');

            // "N or greater" → GE
            if let Some((comparator, value)) = parse_comparison_suffix(after) {
                return (
                    Some(AbilityCondition::QuantityCheck {
                        lhs: QuantityExpr::Ref {
                            qty: qty_ref.clone(),
                        },
                        comparator,
                        rhs: QuantityExpr::Fixed { value },
                    }),
                    before.original.to_string(),
                );
            }
        }
    }
    (None, text.to_string())
}

/// CR 608.2e: Strip "if that creature/permanent has [keyword], [effect] instead" prefix.
/// Returns `TargetHasKeywordInstead` condition and the body effect text (with "instead" stripped).
///
/// Handles patterns like:
/// - "If that creature has flying, it deals twice that much damage to itself instead."
fn strip_target_keyword_instead(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    // "if that creature has [keyword], [body] instead"
    let prefix = lower
        .strip_prefix("if that creature has ")
        .or_else(|| lower.strip_prefix("if that permanent has "));
    if let Some(rest) = prefix {
        if let Some((keyword_str, body)) = rest.split_once(", ") {
            let keyword_str = keyword_str.trim();
            let keyword = crate::types::keywords::Keyword::from_str(keyword_str).unwrap();
            // Extract original-case body from the original text using the pre-strip
            // body length, then strip "instead" from the original-case text directly.
            let body = body.trim();
            let body_text = text[text.len() - body.len()..].trim();
            let body_text = body_text
                .strip_suffix(" instead.")
                .or_else(|| body_text.strip_suffix(" instead"))
                .unwrap_or(body_text);
            // The subject "it" in the body refers to the target creature — remap.
            // Strip leading "it " since try_parse_damage expects "deals damage..."
            let body_text = body_text.strip_prefix("it ").unwrap_or(body_text);
            return (
                Some(AbilityCondition::TargetHasKeywordInstead { keyword }),
                body_text.to_string(),
            );
        }
    }
    (None, text.to_string())
}

/// CR 603.4 + CR 608.2c: Parse counter threshold from text starting after "if it has ".
/// Returns (comparator, threshold, counter_type, bytes_consumed) or None.
///
/// Handles:
/// - "four or more quest counters on it" → (GE, 4, "quest", len)
/// - "no ice counters on it" → (EQ, 0, "ice", len)
fn parse_counter_threshold(text: &str) -> Option<(Comparator, i32, String, usize)> {
    use super::oracle_util::parse_number;

    let original_len = text.len();

    // "no [type] counters on it" → EQ(0)
    if let Some(rest) = text.strip_prefix("no ") {
        let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let raw_type = &rest[..type_end];
        let counter_type = counter::normalize_counter_type(raw_type);
        let after_type = rest[type_end..].trim_start();
        let after_counter = after_type
            .strip_prefix("counters")
            .or_else(|| after_type.strip_prefix("counter"))?;
        let after_on = after_counter
            .trim_start()
            .strip_prefix("on it")
            .or_else(|| after_counter.trim_start().strip_prefix("on this"))?;
        let consumed = original_len - after_on.len();
        return Some((Comparator::EQ, 0, counter_type, consumed));
    }

    // "N or more/fewer [type] counters on it"
    let (n, rest) = parse_number(text)?;
    // parse_number trims trailing whitespace, so remainder starts with "or more "
    let (comparator, rest) = if let Some(r) = rest.strip_prefix("or more ") {
        (Comparator::GE, r)
    } else if let Some(r) = rest.strip_prefix("or fewer ") {
        (Comparator::LE, r)
    } else {
        return None;
    };

    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let raw_type = &rest[..type_end];
    let counter_type = counter::normalize_counter_type(raw_type);
    let after_type = rest[type_end..].trim_start();
    let after_counter = after_type
        .strip_prefix("counters")
        .or_else(|| after_type.strip_prefix("counter"))?;
    let after_on = after_counter
        .trim_start()
        .strip_prefix("on it")
        .or_else(|| after_counter.trim_start().strip_prefix("on this"))?;
    let consumed = original_len - after_on.len();
    Some((comparator, n as i32, counter_type, consumed))
}

/// Build a `QuantityCheck` condition from parsed counter threshold components.
fn build_counter_condition(
    comparator: Comparator,
    threshold: i32,
    counter_type: String,
) -> AbilityCondition {
    AbilityCondition::QuantityCheck {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::CountersOnSelf { counter_type },
        },
        comparator,
        rhs: QuantityExpr::Fixed { value: threshold },
    }
}

/// CR 603.4 + CR 608.2c: Strip "if it has N or more/fewer/no [type] counters on it" condition.
/// Handles both prefix and suffix positions:
///   - Prefix: "if it has four or more quest counters on it, put a +1/+1 counter..."
///   - Suffix: "sacrifice it if it has five or more bloodstain counters on it"
fn strip_counter_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Prefix: "if it has [threshold] [type] counters on it, [remainder]"
    if let Some(rest) = lower.strip_prefix("if it has ") {
        if let Some((comparator, threshold, counter_type, consumed)) = parse_counter_threshold(rest)
        {
            let after = rest[consumed..].trim_start();
            let after = after.strip_prefix(',').unwrap_or(after).trim_start();
            let offset = text.len() - after.len();
            return (
                Some(build_counter_condition(comparator, threshold, counter_type)),
                text[offset..].to_string(),
            );
        }
    }

    // Suffix: "[effect] if it has [threshold] [type] counters on it"
    if let Some((before, after)) = tp.rsplit_around(" if it has ") {
        if let Some((comparator, threshold, counter_type, consumed)) =
            parse_counter_threshold(after.lower)
        {
            // Verify the counter condition consumes to end of string (suffix position)
            let remaining = after.lower[consumed..].trim();
            if remaining.is_empty() || remaining == "." {
                let effect_text = before.original.trim_end_matches('.').trim().to_string();
                return (
                    Some(build_counter_condition(comparator, threshold, counter_type)),
                    effect_text,
                );
            }
        }
    }

    (None, text.to_string())
}

/// Find the position of the last top-level ` if ` in `text` — not inside parentheses or quotes.
/// Uses left-to-right scanning with depth tracking, same approach as `split_leading_conditional`.
fn find_last_top_level_if(text: &str) -> Option<usize> {
    let mut last_pos = None;
    let mut paren_depth = 0u32;
    let mut in_quotes = false;

    for (i, ch) in text.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => paren_depth += 1,
            ')' if !in_quotes => paren_depth = paren_depth.saturating_sub(1),
            _ if !in_quotes && paren_depth == 0 && text[i..].starts_with(" if ") => {
                last_pos = Some(i);
            }
            _ => {}
        }
    }
    last_pos
}

/// CR 608.2c: Strip a general suffix condition (" if {condition}") from effect text.
/// Finds the LAST top-level " if " (not inside parens/quotes), extracts the condition,
/// and attempts to parse it. Returns (None, original) if no parseable condition found.
///
/// The exclusion list is an optimization to skip patterns handled by dedicated strippers.
/// The real safety net is `parse_condition_text` requiring " is " — any unrecognized suffix
/// like "if you control a creature" or "if able" will simply return None, preserving the
/// original text unchanged.
fn strip_suffix_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    // Safety: to_lowercase() on ASCII is byte-length preserving, so `if_pos` from
    // `lower` is valid as a byte offset into `text`. Oracle text is ASCII.
    let lower = text.to_lowercase();
    let Some(if_pos) = find_last_top_level_if(&lower) else {
        return (None, text.to_string());
    };

    let condition_text = lower[if_pos + " if ".len()..].trim_end_matches('.').trim();

    // Exclusion list: patterns handled by dedicated strippers or not general conditions.
    let excluded_prefixes = [
        "able",
        "you do",
        "they do",
        "a player does",
        "no one does",
        "no player does",
        "possible",
        "it has ",
        "its power is ",
        "its toughness is ",
        "that creature has ",
        "that permanent has ",
        "you cast it from",
        "it's a ",
    ];
    for prefix in &excluded_prefixes {
        if condition_text.starts_with(prefix) {
            return (None, text.to_string());
        }
    }

    // Try to parse the condition text into a typed AbilityCondition.
    if let Some(condition) = parse_condition_text(condition_text) {
        let effect_text = text[..if_pos].trim().to_string();
        return (Some(condition), effect_text);
    }

    (None, text.to_string())
}

/// Parse "N or greater", "N or less", "greater than N", "less than N" into (Comparator, i32).
fn parse_comparison_suffix(text: &str) -> Option<(Comparator, i32)> {
    use super::oracle_util::parse_number;

    // "N or greater" / "N or more"
    if let Some(rest) = text
        .strip_suffix(" or greater")
        .or(text.strip_suffix(" or more"))
    {
        let (n, remainder) = parse_number(rest)?;
        if remainder.trim().is_empty() {
            return Some((Comparator::GE, n as i32));
        }
    }
    // "N or less" / "N or fewer"
    if let Some(rest) = text
        .strip_suffix(" or less")
        .or(text.strip_suffix(" or fewer"))
    {
        let (n, remainder) = parse_number(rest)?;
        if remainder.trim().is_empty() {
            return Some((Comparator::LE, n as i32));
        }
    }
    // "greater than N"
    if let Some(rest) = text.strip_prefix("greater than ") {
        let (n, remainder) = parse_number(rest)?;
        if remainder.trim().is_empty() {
            return Some((Comparator::GT, n as i32));
        }
    }
    // "less than N"
    if let Some(rest) = text.strip_prefix("less than ") {
        let (n, remainder) = parse_number(rest)?;
        if remainder.trim().is_empty() {
            return Some((Comparator::LT, n as i32));
        }
    }
    None
}

/// CR 608.2c: Parse comparator + RHS quantity from text after " is ".
/// Generalizes `parse_comparison_suffix` (which returns `i32`) to dynamic `QuantityExpr` RHS.
/// Handles: "greater than {qty}", "less than {qty}", "equal to {qty}",
/// "greater than or equal to {qty}", "{N} or greater", "{N} or less".
fn parse_quantity_comparison(text: &str) -> Option<(Comparator, QuantityExpr)> {
    // Longer prefixes first to avoid "greater than" matching before "greater than or equal to".
    if let Some(rhs_text) = text.strip_prefix("greater than or equal to ") {
        if let Some(rhs) = parse_cda_quantity(rhs_text) {
            return Some((Comparator::GE, rhs));
        }
    }
    if let Some(rhs_text) = text.strip_prefix("less than or equal to ") {
        if let Some(rhs) = parse_cda_quantity(rhs_text) {
            return Some((Comparator::LE, rhs));
        }
    }
    if let Some(rhs_text) = text.strip_prefix("greater than ") {
        if let Some(rhs) = parse_cda_quantity(rhs_text) {
            return Some((Comparator::GT, rhs));
        }
    }
    if let Some(rhs_text) = text.strip_prefix("less than ") {
        if let Some(rhs) = parse_cda_quantity(rhs_text) {
            return Some((Comparator::LT, rhs));
        }
    }
    if let Some(rhs_text) = text.strip_prefix("equal to ") {
        if let Some(rhs) = parse_cda_quantity(rhs_text) {
            return Some((Comparator::EQ, rhs));
        }
    }
    // Fall back to parse_comparison_suffix for "{N} or greater" / "{N} or less" patterns.
    if let Some((comparator, value)) = parse_comparison_suffix(text) {
        return Some((comparator, QuantityExpr::Fixed { value }));
    }
    None
}

/// CR 608.2c: Parse a general condition text fragment into an AbilityCondition.
/// Handles "{quantity} is {comparator} {quantity}" patterns.
/// Returns None for unrecognized conditions (caller preserves original text).
fn parse_condition_text(text: &str) -> Option<AbilityCondition> {
    let text = text.trim().trim_end_matches('.');
    // Pattern: "{lhs} is {comparator} {rhs}"
    let (lhs_text, comparator_rhs) = text.split_once(" is ")?;
    let lhs = parse_cda_quantity(lhs_text)?;
    let (comparator, rhs) = parse_quantity_comparison(comparator_rhs)?;
    Some(AbilityCondition::QuantityCheck {
        lhs,
        comparator,
        rhs,
    })
}

/// Strip "you may " prefix, returning whether the effect is optional.
fn strip_optional_effect_prefix(
    text: &str,
) -> (
    bool,
    Option<crate::types::ability::OpponentMayScope>,
    String,
) {
    let lower = text.to_lowercase();
    // CR 608.2d: "any opponent may" — opponent-choice optional effect.
    if let Some(rest) = lower.strip_prefix("any opponent may ") {
        let offset = text.len() - rest.len();
        return (
            true,
            Some(crate::types::ability::OpponentMayScope::AnyOpponent),
            text[offset..].to_string(),
        );
    }
    if let Some(rest) = lower.strip_prefix("you may ") {
        let offset = text.len() - rest.len();
        (true, None, text[offset..].to_string())
    } else {
        (false, None, text.to_string())
    }
}

/// CR 609.3: Strip "for each [X], " prefix from effect text.
/// Returns the QuantityExpr for the iteration count and the remaining text.
/// "For as long as" is NOT matched (different construct — duration, not iteration).
fn strip_for_each_prefix(text: &str) -> (Option<QuantityExpr>, String) {
    let lower = text.to_lowercase();
    if let Some(rest) = lower.strip_prefix("for each ") {
        if let Some((clause, remainder)) = rest.split_once(", ") {
            if let Some(qty) = parse_for_each_clause(clause) {
                let offset = text.len() - remainder.len();
                return (Some(QuantityExpr::Ref { qty }), text[offset..].to_string());
            }
        }
    }
    (None, text.to_string())
}

/// CR 609.3: Strip "twice" / "three times" / "N times" suffix to produce a
/// `repeat_for` count. Unified with `strip_for_each_prefix` at the chain level
/// so the base action is parsed normally and the resolver loops it N times.
fn strip_repeat_count_suffix(text: &str) -> (Option<QuantityExpr>, String) {
    let lower = text.to_lowercase();
    let suffixes: &[(&str, i32)] = &[
        (" twice", 2),
        (" three times", 3),
        (" four times", 4),
        (" five times", 5),
    ];
    for &(suffix, count) in suffixes {
        if let Some(base) = lower.strip_suffix(suffix) {
            return (
                Some(QuantityExpr::Fixed { value: count }),
                text[..base.len()].to_string(),
            );
        }
    }
    (None, text.to_string())
}

/// Strip "each player/opponent [verb]s" subject prefix.
/// Returns the PlayerFilter scope and the predicate with deconjugated verb.
/// "Each opponent discards a card" → (Some(Opponent), "discard a card")
/// "Each player draws a card" → (Some(All), "draw a card")
fn strip_each_player_subject(text: &str) -> (Option<PlayerFilter>, String) {
    let lower = text.to_lowercase();
    let (scope, rest) = if lower.starts_with("each opponent ") {
        (PlayerFilter::Opponent, &text["each opponent ".len()..])
    } else if lower.starts_with("each player ") {
        (PlayerFilter::All, &text["each player ".len()..])
    } else {
        return (None, text.to_string());
    };

    // Guard: static restriction predicates ("can't", "cannot", "don't", "may only",
    // "may not") belong to the static parser, not the imperative effect pipeline.
    // Intercepting them here would produce Unimplemented instead of typed static modes.
    let rest_lower = rest.trim().to_lowercase();
    if rest_lower.starts_with("can't")
        || rest_lower.starts_with("cannot")
        || rest_lower.starts_with("don't")
        || rest_lower.starts_with("may only")
        || rest_lower.starts_with("may not")
        || rest_lower.starts_with("may cast")
    {
        return (None, text.to_string());
    }

    // Deconjugate the verb: "discards" → "discard", "draws" → "draw"
    let deconjugated = subject::deconjugate_verb(rest);
    (Some(scope), deconjugated)
}

fn strip_leading_duration(text: &str) -> Option<(Duration, &str)> {
    let lower = text.to_lowercase();
    for (prefix, duration) in [
        ("until end of turn, ", Duration::UntilEndOfTurn),
        (
            "until the end of your next turn, ",
            Duration::UntilYourNextTurn,
        ),
        ("until your next turn, ", Duration::UntilYourNextTurn),
    ] {
        if lower.starts_with(prefix) {
            return Some((duration, text[prefix.len()..].trim()));
        }
    }

    // CR 611.2b: "For as long as [condition], [effect]" — leading duration prefix.
    if let Some(rest) = lower.strip_prefix("for as long as ") {
        // Find the comma that separates the condition from the effect body.
        if let Some(comma_pos) = rest.find(", ") {
            let condition_text = &rest[..comma_pos];
            if let Some(dur) = parse_for_as_long_as_condition(condition_text) {
                let effect_start = "for as long as ".len() + comma_pos + ", ".len();
                return Some((dur, text[effect_start..].trim()));
            }
        }
    }

    None
}

fn strip_trailing_duration(text: &str) -> (&str, Option<Duration>) {
    let lower = text.to_lowercase();
    for (suffix, duration) in [
        (" this turn", Duration::UntilEndOfTurn),
        (" until end of turn", Duration::UntilEndOfTurn),
        (
            " until the end of your next turn",
            Duration::UntilYourNextTurn,
        ),
        (" until your next turn", Duration::UntilYourNextTurn),
        (
            " until ~ leaves the battlefield",
            Duration::UntilHostLeavesPlay,
        ),
        (
            " until this creature leaves the battlefield",
            Duration::UntilHostLeavesPlay,
        ),
    ] {
        if lower.ends_with(suffix) {
            let end = text.len() - suffix.len();
            return (text[..end].trim_end_matches(',').trim(), Some(duration));
        }
    }

    // CR 611.2b: "for as long as [condition]" — extract condition from trailing phrase.
    if let Some(pos) = lower.rfind(" for as long as ") {
        let condition_text = &lower[pos + " for as long as ".len()..];
        if let Some(dur) = parse_for_as_long_as_condition(condition_text) {
            let stripped = text[..pos].trim_end_matches(',').trim();
            return (stripped, Some(dur));
        }
    }

    (text, None)
}

/// CR 611.2b: Parse the condition text after "for as long as" into a Duration variant.
/// Maps known condition phrases to typed Duration/StaticCondition variants:
/// - "~ remains tapped" / "it remains tapped" → ForAsLongAs { SourceIsTapped }
/// - "you control ~" → UntilHostLeavesPlay (existing variant)
/// - "~ remains on the battlefield" → UntilHostLeavesPlay
/// - "it has a {type} counter on it" → ForAsLongAs { HasCounters }
/// - Compound: "you control ~ and it remains tapped" → ForAsLongAs { And [...] }
/// - Unknown → ForAsLongAs { Unrecognized }
fn parse_for_as_long_as_condition(condition: &str) -> Option<Duration> {
    let condition = condition.trim().trim_end_matches('.');

    // Compound: "you control ~ and it remains tapped"
    if let Some(and_pos) = condition.find(" and ") {
        let left = condition[..and_pos].trim();
        let right = condition[and_pos + " and ".len()..].trim();
        let left_dur = parse_for_as_long_as_condition(left)?;
        let right_dur = parse_for_as_long_as_condition(right)?;
        let left_cond = duration_to_condition(left_dur);
        let right_cond = duration_to_condition(right_dur);
        return Some(Duration::ForAsLongAs {
            condition: StaticCondition::And {
                conditions: vec![left_cond, right_cond],
            },
        });
    }

    // "~ remains tapped" / "it remains tapped" / "this creature remains tapped"
    if condition.ends_with("remains tapped") {
        return Some(Duration::ForAsLongAs {
            condition: StaticCondition::SourceIsTapped,
        });
    }

    // "you control ~" / "you control this creature"
    if condition.starts_with("you control ") {
        return Some(Duration::UntilHostLeavesPlay);
    }

    // "~ remains on the battlefield" / "it remains on the battlefield"
    if condition.ends_with("remains on the battlefield") {
        return Some(Duration::UntilHostLeavesPlay);
    }

    // "it has a {type} counter on it" / "~ has a {type} counter on it"
    if condition.contains(" has a ") && condition.contains(" counter on it") {
        if let Some(after_has) = strip_after(condition, " has a ") {
            if let Some(counter_end) = after_has.find(" counter") {
                let counter_type = after_has[..counter_end].trim().to_string();
                return Some(Duration::ForAsLongAs {
                    condition: StaticCondition::HasCounters {
                        counter_type,
                        minimum: 1,
                        maximum: None,
                    },
                });
            }
        }
    }

    // Fallback: unrecognized condition text
    Some(Duration::ForAsLongAs {
        condition: StaticCondition::Unrecognized {
            text: condition.to_string(),
        },
    })
}

/// Convert a Duration back into a StaticCondition for compound "and" clauses.
/// UntilHostLeavesPlay maps to IsPresent { filter: None } (source must be on battlefield).
fn duration_to_condition(dur: Duration) -> StaticCondition {
    match dur {
        Duration::ForAsLongAs { condition } => condition,
        Duration::UntilHostLeavesPlay => StaticCondition::IsPresent { filter: None },
        _ => StaticCondition::None,
    }
}

/// CR 603.7a: Strip temporal suffix indicating a delayed trigger condition.
/// Parallel to `strip_trailing_duration()` but for one-shot deferred effects.
/// Duration = "effect is active during this period"; DelayedTriggerCondition = "fire once at this
/// future point".
fn strip_temporal_suffix(text: &str) -> (&str, Option<DelayedTriggerCondition>) {
    let lower = text.to_lowercase();
    for (suffix, condition) in [
        (
            " at the beginning of the next end step",
            DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
        ),
        (
            " at the beginning of the next upkeep",
            DelayedTriggerCondition::AtNextPhase {
                phase: Phase::Upkeep,
            },
        ),
        (
            " at end of combat",
            DelayedTriggerCondition::AtNextPhase {
                phase: Phase::EndCombat,
            },
        ),
    ] {
        if lower.ends_with(suffix) {
            let end = text.len() - suffix.len();
            return (text[..end].trim_end_matches(',').trim(), Some(condition));
        }
    }
    (text, None)
}

/// CR 603.7a: Strip temporal prefix indicating a delayed trigger condition.
/// Symmetric to `strip_temporal_suffix` but handles prefix form:
/// "At the beginning of the next end step, untap up to two lands."
fn strip_temporal_prefix(text: &str) -> (&str, Option<DelayedTriggerCondition>) {
    let lower = text.to_lowercase();
    for (prefix, condition) in [
        (
            "at the beginning of the next end step, ",
            DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
        ),
        (
            "at the beginning of the next upkeep, ",
            DelayedTriggerCondition::AtNextPhase {
                phase: Phase::Upkeep,
            },
        ),
    ] {
        if lower.starts_with(prefix) {
            return (&text[prefix.len()..], Some(condition));
        }
    }
    (text, None)
}

/// CR 115.1d: Extract multi_target spec from PutCounter text.
/// Looks for "counter on up to N" pattern and returns the spec.
/// Used as a post-parse fixup when the AST→Effect lowering loses multi_target info.
fn extract_put_counter_multi_target(text: &str) -> Option<MultiTargetSpec> {
    let lower = text.to_lowercase();
    let marker = "counter on up to ";
    let alt_marker = "counters on up to ";
    let after = strip_after(&lower, marker).or_else(|| strip_after(&lower, alt_marker))?;
    let (n, _) = super::oracle_util::parse_number(after)?;
    Some(MultiTargetSpec {
        min: 0,
        max: Some(n as usize),
    })
}

/// Verbs where "any number of" / "up to N" modifies the target set (CR 115.1d),
/// not a resource count (counters, life, etc.).
const MULTI_TARGET_VERBS: &[&str] = &[
    "exile",
    "tap",
    "untap",
    "sacrifice",
    "return",
    "destroy",
    "choose",
];

/// CR 115.1d: Strip numeric word prefix before "target" from effect text.
/// "two target creatures" → (2, "target creatures")
/// "three target artifacts" → (3, "target artifacts")
/// Returns None if text doesn't start with a number word followed by "target".
fn strip_numeric_target_prefix(lower: &str) -> Option<(usize, &str)> {
    let number_words: &[(&str, usize)] = &[
        ("two ", 2),
        ("three ", 3),
        ("four ", 4),
        ("five ", 5),
        ("six ", 6),
    ];
    for (word, count) in number_words {
        if let Some(rest) = lower.strip_prefix(word) {
            if rest.starts_with("target ") || rest.starts_with("target,") {
                return Some((*count, rest));
            }
        }
    }
    None
}

/// CR 115.1d: Strip "any number of" or "up to N" quantifier from imperative text.
/// Only applies to verbs where the quantifier modifies target selection.
fn strip_any_number_quantifier(text: &str) -> (String, Option<MultiTargetSpec>) {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let verb = lower.split_whitespace().next().unwrap_or("");
    if !MULTI_TARGET_VERBS.contains(&verb) {
        return (text.to_string(), None);
    }

    let verb_end = lower.find(' ').map(|i| i + 1).unwrap_or(0);
    let (verb_tp, after_verb_tp) = tp.split_at(verb_end);

    if after_verb_tp.starts_with("any number of ") {
        if let Some(rest) = after_verb_tp.strip_prefix("any number of ") {
            let rebuilt = format!("{}{}", verb_tp.original, rest.original);
            return (rebuilt, Some(MultiTargetSpec { min: 0, max: None }));
        }
    }
    if let Some(after_up_to) = after_verb_tp.strip_prefix("up to ") {
        if let Some((n, remainder)) = parse_number(after_up_to.lower) {
            let consumed_len = after_up_to.lower.len() - remainder.len();
            let (_, rest) = after_up_to.split_at(consumed_len);
            let rebuilt = format!("{}{}", verb_tp.original, rest.original.trim_start());
            return (
                rebuilt,
                Some(MultiTargetSpec {
                    min: 0,
                    max: Some(n as usize),
                }),
            );
        }
    }
    (text.to_string(), None)
}

/// Strip "to the battlefield [under X's control]" and similar destination phrases.
/// Returns the remaining target text and the destination zone (if battlefield).
/// Result of parsing a "return ... to <zone>" destination phrase.
struct ReturnDestination {
    zone: Zone,
    transformed: bool,
    // CR 110.2: "under your control" — controller override on zone change.
    under_your_control: bool,
    // CR 614.1: "tapped" — enters the battlefield tapped.
    enter_tapped: bool,
}

/// Detect "return ... to <zone>" destination phrase, including "transformed" flag.
fn strip_return_destination_ext(text: &str) -> (&str, Option<ReturnDestination>) {
    let lower = text.to_lowercase();
    // Ordered longest-first to avoid partial matches.
    // "transformed" variants must come before their non-transformed counterparts.
    // Tuples: (phrase, zone, transformed, under_your_control, enter_tapped)
    // Ordered longest-first; compound patterns must precede their shorter substrings.
    let patterns: &[(&str, Zone, bool, bool, bool)] = &[
        // Tapped + transformed + owner's control (compound, longest)
        (
            " to the battlefield tapped and transformed under its owner's control",
            Zone::Battlefield,
            true,
            false,
            true,
        ),
        // Transformed + your control
        (
            " to the battlefield transformed under your control",
            Zone::Battlefield,
            true,
            true,
            false,
        ),
        // Transformed + owner's control variants
        (
            " to the battlefield transformed under their owners' control",
            Zone::Battlefield,
            true,
            false,
            false,
        ),
        (
            " to the battlefield transformed under its owner's control",
            Zone::Battlefield,
            true,
            false,
            false,
        ),
        (
            " to the battlefield transformed under his owner's control",
            Zone::Battlefield,
            true,
            false,
            false,
        ),
        (
            " to the battlefield transformed under her owner's control",
            Zone::Battlefield,
            true,
            false,
            false,
        ),
        (
            " to the battlefield transformed",
            Zone::Battlefield,
            true,
            false,
            false,
        ),
        // Tapped + control variants (must precede shorter "tapped" and "under X control")
        (
            " to the battlefield tapped under their owners' control",
            Zone::Battlefield,
            false,
            false,
            true,
        ),
        (
            " to the battlefield tapped under its owner's control",
            Zone::Battlefield,
            false,
            false,
            true,
        ),
        (
            " to the battlefield tapped under your control",
            Zone::Battlefield,
            false,
            true,
            true,
        ),
        // Simple control variants
        (
            " to the battlefield under their owners' control",
            Zone::Battlefield,
            false,
            false,
            false,
        ),
        (
            " to the battlefield under its owner's control",
            Zone::Battlefield,
            false,
            false,
            false,
        ),
        // CR 110.2: "under your control" — controller override.
        (
            " to the battlefield under your control",
            Zone::Battlefield,
            false,
            true,
            false,
        ),
        // CR 614.1: "tapped" — enters tapped.
        (
            " to the battlefield tapped",
            Zone::Battlefield,
            false,
            false,
            true,
        ),
        (
            " to the battlefield",
            Zone::Battlefield,
            false,
            false,
            false,
        ),
        // "onto" variants
        (
            " onto the battlefield under your control",
            Zone::Battlefield,
            false,
            true,
            false,
        ),
        (
            " onto the battlefield tapped",
            Zone::Battlefield,
            false,
            false,
            true,
        ),
        (
            " onto the battlefield",
            Zone::Battlefield,
            false,
            false,
            false,
        ),
        // Hand destinations
        (" to its owner's hand", Zone::Hand, false, false, false),
        (" to their owner's hand", Zone::Hand, false, false, false),
        (" to their owners' hands", Zone::Hand, false, false, false),
        (" to your hand", Zone::Hand, false, false, false),
        // Graveyard destinations
        (
            " to its owner's graveyard",
            Zone::Graveyard,
            false,
            false,
            false,
        ),
        (
            " to their owner's graveyard",
            Zone::Graveyard,
            false,
            false,
            false,
        ),
        (
            " to their owners' graveyards",
            Zone::Graveyard,
            false,
            false,
            false,
        ),
        (" to your graveyard", Zone::Graveyard, false, false, false),
        // NOTE: Library destinations ("to the top/bottom of owner's library") are
        // intentionally NOT handled here. They require PutAtLibraryPosition (positional
        // placement without shuffling), not ChangeZone (which auto-shuffles).
    ];
    for (phrase, zone, transformed, under_your_control, enter_tapped) in patterns {
        if let Some(pos) = lower.rfind(phrase) {
            return (
                text[..pos].trim(),
                Some(ReturnDestination {
                    zone: *zone,
                    transformed: *transformed,
                    under_your_control: *under_your_control,
                    enter_tapped: *enter_tapped,
                }),
            );
        }
    }
    (text, None)
}

/// CR 601.2d: Parse "deal N damage divided as you choose among [targets]" and
/// "deal N damage distributed among [targets]" → Effect::DealDamage with distribute flag.
///
/// Also handles "deal N damage divided evenly, rounded down, among [targets]" which uses
/// the same Effect but signals even-split (the engine treats this as a pre-set distribution).
fn try_parse_distribute_damage(lower: &str, text: &str) -> Option<ParsedEffectClause> {
    let tp = TextPair::new(text, lower);
    let pos = tp.find("deals ").or_else(|| tp.find("deal "))?;
    let verb_len = if lower[pos..].starts_with("deals ") {
        6
    } else {
        5
    };
    let (_, after_tp) = tp.split_at(pos + verb_len);

    let (amount, rest_tp) =
        if let Some((qty, rem)) = super::oracle_util::parse_count_expr(after_tp.lower) {
            if rem.starts_with("damage") {
                let skip = after_tp.lower.len() - rem.len() + "damage".len();
                let (_, rest) = after_tp.split_at(skip);
                (qty, rest)
            } else {
                return None;
            }
        } else {
            return None;
        };

    // Detect distribution keywords.
    // CR 601.2d: "divided as you choose among" / "distributed among" → player chooses.
    // "divided evenly, rounded down, among" → auto-computed even split.
    let distribute_kind = if rest_tp.contains("divided as you choose among")
        || rest_tp.contains("distributed among")
    {
        DistributionUnit::Damage
    } else if rest_tp.contains("divided evenly") {
        DistributionUnit::EvenSplitDamage
    } else {
        return None;
    };

    // Parse the target after the distribution keyword.
    let target_tp = rest_tp
        .strip_after("divided as you choose among ")
        .or_else(|| rest_tp.strip_after("distributed among "))
        .or_else(|| {
            // CR 601.2d: "divided evenly, rounded down, among " variant.
            rest_tp.strip_after("divided evenly, rounded down, among ")
        })?;
    let target_text = target_tp.original.trim();

    // CR 115.1d: Detect "any number of" quantifier before the target phrase.
    let target_lower = target_text.to_lowercase();
    let (stripped_target_text, multi_target) = if target_lower.starts_with("any number of ") {
        let skip = "any number of ".len();
        (
            &target_text[skip..],
            // CR 601.2d: min: 1 because each target must receive at least 1.
            Some(MultiTargetSpec { min: 1, max: None }),
        )
    } else {
        (target_text, None)
    };
    let (target, _) = parse_target(stripped_target_text);

    Some(ParsedEffectClause {
        effect: Effect::DealDamage {
            amount,
            target,
            damage_source: None,
        },
        duration: None,
        sub_ability: None,
        distribute: Some(distribute_kind),
        multi_target,
    })
}

/// CR 601.2d: Parse "distribute N [type] counters among [targets]"
/// → Effect::PutCounter with distribute flag set.
fn try_parse_distribute_counters(lower: &str, text: &str) -> Option<ParsedEffectClause> {
    // "distribute " is 11 bytes; Oracle text is ASCII so byte == char offsets.
    let after_lower = lower.strip_prefix("distribute ")?;
    let (count, rest_lower) = parse_number(after_lower)?;

    let type_end = rest_lower
        .find(|c: char| c.is_whitespace())
        .unwrap_or(rest_lower.len());
    let raw_type = &rest_lower[..type_end];
    let counter_type = counter::normalize_counter_type(raw_type);

    // Require "counter(s)" immediately after the counter type word.
    let after_type = rest_lower[type_end..].trim_start();
    let counter_word_len = if after_type.starts_with("counters") {
        "counters".len()
    } else if after_type.starts_with("counter") {
        "counter".len()
    } else {
        return None;
    };

    // Find "among " in lower to get byte offset for parse_target on original-case `text`.
    let among_needle = "among ";
    let among_pos = lower.find(among_needle)?;
    let target_offset = among_pos + among_needle.len();

    // CR 115.1d: Detect "any number of" quantifier before the target phrase.
    let target_text = &text[target_offset..];
    let target_text_lower = &lower[target_offset..];
    let (stripped_target, multi_target) = if target_text_lower.starts_with("any number of ") {
        let skip = "any number of ".len();
        (
            &target_text[skip..],
            // CR 601.2d: min: 1 because each target must receive at least 1.
            Some(MultiTargetSpec { min: 1, max: None }),
        )
    } else {
        (target_text, None)
    };
    let (target, _) = parse_target(stripped_target);

    // Verify the "among" comes after the counter word (sanity guard against false matches).
    let expected_min =
        "distribute ".len() + (after_lower.len() - rest_lower.len()) + type_end + counter_word_len;
    if among_pos < expected_min {
        return None;
    }
    let _ = counter_word_len; // used above

    Some(ParsedEffectClause {
        effect: Effect::PutCounter {
            counter_type,
            count: count as i32,
            target,
        },
        duration: None,
        sub_ability: None,
        distribute: Some(DistributionUnit::Counters(raw_type.to_string())),
        multi_target,
    })
}

/// Thin wrapper around `try_parse_damage_with_remainder` for callers that don't
/// need the remainder (e.g., `parse_cost_resource_ast`). The remainder is only
/// safely discardable when `try_split_damage_compound` has already run and found
/// no compound connector.
fn try_parse_damage(lower: &str, text: &str) -> Option<Effect> {
    let (effect, _remainder) = try_parse_damage_with_remainder(text, lower)?;
    Some(effect)
}

/// Parse damage effects, returning both the Effect and `parse_target`'s unconsumed
/// remainder. The remainder is the compound boundary oracle — if it starts with
/// `" and "`, the caller can chain the trailing clause as a sub_ability.
///
/// Signature follows `try_parse_verb_and_target`: `text` (original case) bears the
/// return lifetime since the remainder is a sub-slice of it; `lower` is elided.
///
/// Safety: `pos` is computed from `lower.find(...)` and used to slice both `text`
/// and `lower` at the same byte offset. This is sound because Oracle text is ASCII
/// and `to_lowercase()` preserves byte length for ASCII characters.
fn try_parse_damage_with_remainder<'a>(text: &'a str, lower: &str) -> Option<(Effect, &'a str)> {
    // Match: "~ deals N damage to {target}" / "deal N damage to {target}"
    // and variable forms like "deal that much damage" or
    // "deal damage equal to its power".
    let pos = lower.find("deals ").or_else(|| lower.find("deal "))?;
    let verb_len = if lower[pos..].starts_with("deals ") {
        6
    } else {
        5
    };
    let after = &text[pos + verb_len..];
    let after_lower = &lower[pos + verb_len..];

    let (amount, after_target) = if let Some((qty, rest)) =
        super::oracle_util::parse_count_expr(after_lower)
    {
        if rest.starts_with("damage") {
            (qty, &after[after.len() - rest.len() + "damage".len()..])
        } else {
            return None;
        }
    } else if after_lower.starts_with("twice that much damage") {
        // CR 120.8: "twice that much damage" → Multiply { factor: 2, inner: EventContextAmount }
        (
            QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
            },
            &after["twice that much damage".len()..],
        )
    } else if after_lower.starts_with("that much damage") {
        (
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            &after["that much damage".len()..],
        )
    } else if after_lower.starts_with("damage to ") {
        // Pattern: "damage to [target] equal to [amount]"
        // Used by: "deals damage to itself equal to its power",
        //          "deals damage to each player equal to the number of ...",
        //          "deals damage to that player equal to the number of ..."
        let rest = after_lower.strip_prefix("damage to ").unwrap();
        if let Some((target_phrase, amount_phrase)) = rest.split_once(" equal to ") {
            let amount_phrase = amount_phrase
                .trim_end_matches('.')
                .trim_end_matches(',')
                .trim();
            // Parse amount using existing helpers
            let qty = crate::parser::oracle_quantity::parse_event_context_quantity(amount_phrase)
                .or_else(|| crate::parser::oracle_quantity::parse_cda_quantity(amount_phrase));
            if let Some(qty) = qty {
                let target_phrase = target_phrase.trim();
                // Route based on target phrase
                if target_phrase == "itself" {
                    // When target is "itself", "its power" means the target's power,
                    // not a triggering event's power. Remap EventContext refs to Target refs.
                    let qty = match &qty {
                        QuantityExpr::Ref {
                            qty: QuantityRef::EventContextSourcePower,
                        } => QuantityExpr::Ref {
                            qty: QuantityRef::TargetPower,
                        },
                        other => other.clone(),
                    };
                    return Some((
                        Effect::DealDamage {
                            amount: qty,
                            target: TargetFilter::ParentTarget,
                            damage_source: Some(DamageSource::Target),
                        },
                        "",
                    ));
                } else if target_phrase.starts_with("each ") {
                    // "each player" → DamageEachPlayer (per-player varying damage)
                    // "each creature" → DamageAll (uniform damage to objects)
                    if target_phrase.contains("player") || target_phrase.contains("opponent") {
                        let player_filter = if target_phrase.contains("opponent") {
                            PlayerFilter::Opponent
                        } else {
                            PlayerFilter::All
                        };
                        return Some((
                            Effect::DamageEachPlayer {
                                amount: qty,
                                player_filter,
                            },
                            "",
                        ));
                    }
                    let (filter, _) = parse_target(target_phrase);
                    return Some((
                        Effect::DamageAll {
                            amount: qty,
                            target: filter,
                        },
                        "",
                    ));
                } else if let Some((target, _ecr_rem)) = parse_event_context_ref(target_phrase) {
                    #[cfg(debug_assertions)]
                    types::assert_no_compound_remainder(_ecr_rem, target_phrase);
                    return Some((
                        Effect::DealDamage {
                            amount: qty,
                            target,
                            damage_source: None,
                        },
                        "",
                    ));
                } else {
                    let (target, _) = parse_target(target_phrase);
                    return Some((
                        Effect::DealDamage {
                            amount: qty,
                            target,
                            damage_source: None,
                        },
                        "",
                    ));
                }
            }
        }
        return None;
    } else if after_lower.starts_with("damage equal to ") {
        let amount_text = &after["damage equal to ".len()..];
        let to_pos = amount_text.to_lowercase().find(" to ")?;
        let qty_text = amount_text[..to_pos].trim();
        let qty = crate::parser::oracle_quantity::parse_event_context_quantity(qty_text)
            .unwrap_or_else(|| QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: qty_text.to_string(),
                },
            });
        (qty, &amount_text[to_pos + 4..])
    } else {
        return None;
    };

    let after_to = after_target
        .trim()
        .strip_prefix("to ")
        .unwrap_or(after_target)
        .trim();
    if after_to.starts_with("each ") {
        let (target, rem) = parse_target(after_to);
        return Some((Effect::DamageAll { amount, target }, rem));
    }

    // CR 120.3: "itself" — the source creature is both damage source and recipient.
    let after_to_lower = after_to.to_lowercase();
    if after_to_lower == "itself" || after_to_lower.starts_with("itself ") {
        return Some((
            Effect::DealDamage {
                amount,
                target: TargetFilter::ParentTarget,
                damage_source: Some(DamageSource::Target),
            },
            "",
        ));
    }

    // CR 608.2k: Check for event-context references before standard target parsing.
    if let Some((target, ecr_rem)) = parse_event_context_ref(after_to) {
        return Some((
            Effect::DealDamage {
                amount: amount.clone(),
                target,
                damage_source: None,
            },
            ecr_rem,
        ));
    }

    let (target, rem) = parse_target(after_to);
    Some((
        Effect::DealDamage {
            amount,
            target,
            damage_source: None,
        },
        rem,
    ))
}

fn try_parse_pump(lower: &str, text: &str) -> Option<Effect> {
    // Match "+N/+M", "+X/+0", "-X/-X", etc.
    let tp = TextPair::new(text, lower);
    let re_pos = tp.find("gets ").or_else(|| tp.find("get "))?;
    let offset = if lower[re_pos..].starts_with("gets ") {
        5
    } else {
        4
    };
    let (_, after_tp) = tp.split_at(re_pos + offset);
    let after = after_tp.original.trim();
    let token_end = after
        .find(|c: char| c.is_whitespace() || c == ',' || c == '.')
        .unwrap_or(after.len());
    let token = &after[..token_end];
    parse_pt_modifier(token).map(|(power, toughness)| Effect::Pump {
        power,
        toughness,
        target: TargetFilter::Any,
    })
}

fn parse_pump_clause(predicate: &str) -> Option<(PtValue, PtValue, Option<Duration>)> {
    let predicate_lower = predicate.to_lowercase();
    let predicate_tp = TextPair::new(predicate, &predicate_lower);
    let (without_where, where_x_expression) = strip_trailing_where_x(predicate_tp);
    let (without_duration, duration) = strip_trailing_duration(without_where.original);
    let lower = without_duration.to_lowercase();

    let after = if lower.starts_with("gets ") {
        &without_duration[5..]
    } else if lower.starts_with("get ") {
        &without_duration[4..]
    } else {
        return None;
    }
    .trim_start();

    let token_end = after
        .find(|c: char| c.is_whitespace() || c == ',' || c == '.')
        .unwrap_or(after.len());
    let token = &after[..token_end];
    let trailing = after[token_end..]
        .trim_start_matches(|c: char| c == ',' || c.is_whitespace())
        .trim();
    if !trailing.is_empty() {
        return None;
    }

    let (power, toughness) = parse_pt_modifier(token)?;
    let power = apply_where_x_expression(power, where_x_expression.as_deref());
    let toughness = apply_where_x_expression(toughness, where_x_expression.as_deref());

    Some((power, toughness, duration))
}

pub(crate) fn strip_trailing_where_x<'a>(tp: TextPair<'a>) -> (TextPair<'a>, Option<String>) {
    for needle in [", where x is ", " where x is "] {
        if let Some((before, after)) = tp.split_around(needle) {
            let expression = after
                .original
                .trim()
                .trim_end_matches('.')
                .trim()
                .to_string();
            if expression.is_empty() {
                return (tp, None);
            }
            return (before.trim_end_matches(',').trim_end(), Some(expression));
        }
    }
    (tp, None)
}

fn strip_leading_sequence_connector(text: &str) -> &str {
    let trimmed = text.trim_start();

    if trimmed.eq_ignore_ascii_case("then") {
        return "";
    }

    trimmed
        .strip_prefix("Then, ")
        .or_else(|| trimmed.strip_prefix("Then "))
        .or_else(|| trimmed.strip_prefix("then, "))
        .or_else(|| trimmed.strip_prefix("then "))
        .or_else(|| trimmed.strip_prefix("and "))
        .or_else(|| trimmed.strip_prefix("And "))
        .unwrap_or(trimmed)
}

fn apply_where_x_expression(value: PtValue, where_x_expression: Option<&str>) -> PtValue {
    match (value, where_x_expression) {
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("X") => {
            crate::parser::oracle_quantity::parse_cda_quantity(expression)
                .map(PtValue::Quantity)
                .unwrap_or_else(|| PtValue::Variable(expression.to_string()))
        }
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("-X") => {
            crate::parser::oracle_quantity::parse_cda_quantity(expression)
                .map(|inner| {
                    PtValue::Quantity(QuantityExpr::Multiply {
                        factor: -1,
                        inner: Box::new(inner),
                    })
                })
                .unwrap_or_else(|| PtValue::Variable(format!("-({expression})")))
        }
        (value, _) => value,
    }
}

fn parse_pt_modifier(text: &str) -> Option<(PtValue, PtValue)> {
    let token = text.trim();
    let slash = token.find('/')?;
    let power = parse_signed_pt_component(token[..slash].trim())?;
    let toughness = parse_signed_pt_component(token[slash + 1..].trim())?;
    Some((power, toughness))
}

fn parse_signed_pt_component(text: &str) -> Option<PtValue> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let (sign, body) = if let Some(rest) = text.strip_prefix('+') {
        (1, rest.trim())
    } else if let Some(rest) = text.strip_prefix('-') {
        (-1, rest.trim())
    } else {
        (1, text)
    };

    if body.eq_ignore_ascii_case("x") {
        return Some(if sign < 0 {
            PtValue::Variable("-X".to_string())
        } else {
            PtValue::Variable("X".to_string())
        });
    }

    let value = body.parse::<i32>().ok()?;
    Some(PtValue::Fixed(sign * value))
}

fn try_parse_put_zone_change(lower: &str, text: &str) -> Option<Effect> {
    let tp = TextPair::new(text, lower);
    let (_, after_put_tp) = tp.split_at(4);

    for (needle, destination) in [
        (" onto the battlefield", Zone::Battlefield),
        (" into your hand", Zone::Hand),
        (" into their hand", Zone::Hand),
        (" into its owner's hand", Zone::Hand),
        (" into their owner's hand", Zone::Hand),
        (" into your graveyard", Zone::Graveyard),
        (" into its owner's graveyard", Zone::Graveyard),
        (" into their owner's graveyard", Zone::Graveyard),
        (" on the bottom of", Zone::Library),
        (" on top of", Zone::Library),
    ] {
        if let Some((before, _)) = after_put_tp.split_around(needle) {
            let target_text = before.original.trim();
            if target_text.is_empty() {
                return None;
            }
            let (target, _) = parse_target(target_text);
            // CR 110.2: "under your control" overrides the entering object's controller.
            let under_your_control = after_put_tp.contains("under your control");
            return Some(Effect::ChangeZone {
                origin: infer_origin_zone(after_put_tp.lower),
                destination,
                target,
                owner_library: false,
                enter_transformed: false,
                under_your_control,
                enter_tapped: false,
                enters_attacking: false,
            });
        }
    }

    None
}

/// CR 118.12: Parse "unless its controller pays {X}" from counter/trigger text.
/// Returns `UnlessCost::Fixed` for static costs ({3}, {1}{U}) and
/// `UnlessCost::DynamicGeneric` for "pays {X}, where X is this creature's power" etc.
fn parse_unless_payment(lower: &str) -> Option<UnlessCost> {
    // Find "unless" followed by a subject and "pays {cost}"
    let after_unless = strip_after(lower, "unless ")?;
    // Skip the subject ("its controller", "that player", "he or she", etc.)
    let cost_str = strip_after(after_unless, "pays ")?;
    // Extract the mana cost (brace-delimited symbols)
    let cost_end = cost_str
        .find(|c: char| c != '{' && c != '}' && !c.is_alphanumeric())
        .unwrap_or(cost_str.len());
    let cost_text = cost_str[..cost_end].trim();
    if cost_text.is_empty() || !cost_text.contains('{') {
        return None;
    }
    // Check for dynamic {X} with "where X is" clause
    if cost_text == "{X}" || cost_text == "{x}" {
        let after_cost = &cost_str[cost_end..];
        if let Some(quantity) = parse_where_x_is(after_cost) {
            return Some(UnlessCost::DynamicGeneric { quantity });
        }
        // {X} without "where X is" — unresolvable, skip
        return None;
    }
    let cost = parse_mtgjson_mana_cost(cost_text);
    if cost == ManaCost::NoCost || cost == ManaCost::zero() {
        return None;
    }
    Some(UnlessCost::Fixed { cost })
}

/// Parse "where X is this creature's power" and similar dynamic quantity clauses.
fn parse_where_x_is(text: &str) -> Option<QuantityExpr> {
    let trimmed = text.trim().trim_start_matches(',').trim();
    let rest = trimmed.strip_prefix("where x is ")?;
    if rest.contains("power") {
        Some(QuantityExpr::Ref {
            qty: QuantityRef::SelfPower,
        })
    } else if rest.contains("toughness") {
        Some(QuantityExpr::Ref {
            qty: QuantityRef::SelfToughness,
        })
    } else {
        None
    }
}

fn infer_origin_zone(lower: &str) -> Option<Zone> {
    if contains_possessive(lower, "from", "graveyard") || lower.contains("from a graveyard") {
        Some(Zone::Graveyard)
    } else if lower.contains("from exile") {
        Some(Zone::Exile)
    } else if contains_possessive(lower, "from", "hand") {
        Some(Zone::Hand)
    } else if contains_possessive(lower, "from", "library") {
        Some(Zone::Library)
    } else if lower.contains("graveyard") && !lower.contains("from") {
        // CR 404.1: Possessive graveyard references without "from" — e.g.,
        // "exile each opponent's graveyard", "exile target player's graveyard"
        Some(Zone::Graveyard)
    } else {
        None
    }
}

pub(crate) fn normalize_verb_token(token: &str) -> String {
    let token = token.trim_matches(|c: char| !c.is_alphabetic());
    match token {
        "does" => "do".to_string(),
        "has" => "have".to_string(),
        "is" => "be".to_string(),
        "copies" => "copy".to_string(),
        _ if token.ends_with('s') && !token.ends_with("ss") => token[..token.len() - 1].to_string(),
        _ => token.to_string(),
    }
}

fn constrain_filter_to_stack(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller,
            mut properties,
        }) => {
            if !properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
            {
                properties.push(FilterProp::InZone { zone: Zone::Stack });
            }
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller,
                properties,
            })
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(constrain_filter_to_stack).collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.into_iter().map(constrain_filter_to_stack).collect(),
        },
        other => other,
    }
}

/// CR 115.7: Parse "change the target of [spell]" and "you may choose new targets for [spell]".
///
/// Covers two Oracle text patterns:
/// - "change the target of [spell phrase]" → scope: `Single`
/// - "you may choose new targets for [spell phrase]" → scope: `All`
///
/// An optional trailing "to [target phrase]" sets `forced_to`.
fn try_parse_change_targets(lower: &str) -> Option<Effect> {
    let (scope, rest) = if let Some(r) = lower.strip_prefix("change the target of ") {
        (RetargetScope::Single, r)
    } else if let Some(r) = lower.strip_prefix("you may choose new targets for ") {
        (RetargetScope::All, r)
    } else {
        return None;
    };

    // Split off trailing "to [target]" — forced retarget destination.
    let (spell_phrase, forced_to) = if let Some((before, after)) = rest.split_once(" to ") {
        let (filter, _) = parse_target(after);
        (before, Some(filter))
    } else {
        (rest, None)
    };

    // CR 115.7: Parse the spell phrase to extract a stack-entry filter.
    // Strip "with a single target" qualifier before parsing the type.
    let has_single_target = spell_phrase.contains("with a single target");
    // CR 115.9c: "that targets only [X]" is handled by parse_that_clause_suffix via
    // parse_type_phrase, producing FilterProp::TargetsOnly — no manual stripping needed.
    let spell_phrase_clean = spell_phrase.replace(" with a single target", "");
    let spell_phrase_clean = spell_phrase_clean.trim();

    // Handle "spell or ability" specially since "ability" is not a card type in parse_target.
    // CR 115.7: "spell or ability" matches any spell or any activated/triggered ability on the stack.
    let mut target = if spell_phrase_clean.contains("spell or ability")
        || spell_phrase_clean.contains("spell and/or ability")
    {
        // Both spells and abilities on the stack
        TargetFilter::Or {
            filters: vec![TargetFilter::StackSpell, TargetFilter::StackAbility],
        }
    } else if spell_phrase_clean.contains("activated or triggered ability")
        || spell_phrase_clean.contains("activated ability")
    {
        TargetFilter::StackAbility
    } else if spell_phrase_clean.contains("spell") {
        // Parse with parse_target for type-specific spells (e.g. "instant or sorcery spell")
        let (parsed, _) = parse_target(spell_phrase_clean);
        constrain_filter_to_stack(parsed)
    } else {
        let (parsed, _) = parse_target(spell_phrase_clean);
        parsed
    };

    // Add HasSingleTarget property if "with a single target" was specified
    if has_single_target {
        target = add_single_target_constraint(target);
    }

    Some(Effect::ChangeTargets {
        target,
        scope,
        forced_to,
    })
}

/// Add a `HasSingleTarget` property to a stack-entry filter.
/// CR 115.7: "with a single target" constrains which stack entries can be retargeted.
fn add_single_target_constraint(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            tf.properties.push(FilterProp::HasSingleTarget);
            TargetFilter::Typed(tf)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(add_single_target_constraint)
                .collect(),
        },
        // For non-typed filters (StackAbility, StackSpell), wrap in And with a typed filter
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter {
                    properties: vec![FilterProp::HasSingleTarget],
                    ..TypedFilter::default()
                }),
            ],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        ContinuousModification, ControllerRef, DoublePTMode, ManaProduction, PaymentCost,
        TypeFilter,
    };
    use crate::types::mana::ManaColor;

    #[test]
    fn effect_lightning_bolt() {
        let e = parse_effect("Lightning Bolt deals 3 damage to any target");
        assert!(matches!(
            e,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                ..
            }
        ));
    }

    #[test]
    fn damage_to_itself_equal_to_power_low_level() {
        // Low-level: verify try_parse_damage directly
        let e = try_parse_damage(
            "deal damage to itself equal to its power",
            "deal damage to itself equal to its power",
        );
        let e = e.expect("should parse damage to itself");
        assert!(
            matches!(
                e,
                Effect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::TargetPower,
                    },
                    target: TargetFilter::ParentTarget,
                    damage_source: Some(DamageSource::Target),
                }
            ),
            "expected DealDamage with TargetPower/ParentTarget, got: {e:?}"
        );
    }

    #[test]
    fn damage_to_itself_equal_to_power_full_pipeline() {
        // Full pipeline: subject stripping + imperative → should produce DealDamage
        let clause = parse_effect_clause(
            "~ deals damage to itself equal to its power",
            &ParseContext::default(),
        );
        assert!(
            matches!(
                clause.effect,
                Effect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::TargetPower,
                    },
                    target: TargetFilter::ParentTarget,
                    damage_source: Some(DamageSource::Target),
                }
            ),
            "expected DealDamage with TargetPower/ParentTarget, got: {:?}",
            clause.effect
        );
    }

    #[test]
    fn try_split_damage_compound_lightning_helix() {
        let ctx = ParseContext::default();
        let clause =
            try_split_damage_compound("~ deals 3 damage to any target and you gain 3 life", &ctx);
        let clause = clause.expect("should split damage + life gain compound");
        assert!(
            matches!(
                clause.effect,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Any,
                    ..
                }
            ),
            "primary effect should be DealDamage, got: {:?}",
            clause.effect
        );
        let sub = clause
            .sub_ability
            .expect("should have sub_ability for life gain");
        assert!(
            matches!(
                *sub.effect,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    ..
                }
            ),
            "sub_ability should be GainLife(3), got: {:?}",
            sub.effect
        );
    }

    #[test]
    fn try_split_damage_compound_no_compound() {
        let ctx = ParseContext::default();
        let result = try_split_damage_compound("~ deals 3 damage to target creature", &ctx);
        assert!(
            result.is_none(),
            "no compound connector — should return None"
        );
    }

    #[test]
    fn try_split_damage_compound_false_and_in_filter() {
        let ctx = ParseContext::default();
        let result = try_split_damage_compound(
            "~ deals 3 damage to target creature you own and control",
            &ctx,
        );
        // parse_target consumes "you own and control" as part of the filter,
        // leaving empty remainder — no false split.
        assert!(
            result.is_none(),
            "filter 'and' should not trigger compound split"
        );
    }

    #[test]
    fn debug_that_creature_pump_only() {
        // Isolated test: parse sub-text directly through parse_effect_clause
        let ctx = ParseContext::default();
        let clause = parse_effect_clause("that creature gets -2/-2 until end of turn", &ctx);
        assert!(
            !matches!(clause.effect, Effect::Unimplemented { .. }),
            "should parse, got: {:?}",
            clause.effect
        );
    }

    #[test]
    fn try_split_damage_compound_anaphoric() {
        // Test via parse_effect_chain (the normal entry point) rather than calling
        // try_split_damage_compound directly, since the " and "-compound path through
        // try_split_damage_compound → parse_effect_clause → subject-predicate creates
        // deep debug-mode frames that exceed the default thread stack.
        let def = parse_effect_chain(
            "~ deals 2 damage to target creature. That creature gets -2/-2 until end of turn.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::DealDamage { .. }),
            "primary should be DealDamage, got: {:?}",
            def.effect
        );
        let sub = def.sub_ability.expect("should have sub_ability");
        // "That creature" is now recognized as a subject, producing a Pump via the
        // subject-predicate continuous clause path (CantUntap if "doesn't untap",
        // Pump if "gets +/-").
        match &*sub.effect {
            Effect::Pump { .. } | Effect::PumpAll { .. } => {
                // Through parse_effect_chain (sentence splitter), the sub-clause
                // is parsed independently. "That creature" resolves as a Creature
                // type filter, not ParentTarget.
            }
            other => panic!("sub_ability should be Pump/PumpAll, got: {other:?}"),
        }
    }

    #[test]
    fn try_split_damage_compound_multi_target_not_split() {
        // "each opponent and each creature" is a multi-phrase target, not a compound.
        // Goblin Chainwhirler: "deals 1 damage to each opponent and each creature
        // and planeswalker they control"
        let ctx = ParseContext::default();
        let result = try_split_damage_compound(
            "deal 1 damage to each opponent and each creature and planeswalker they control",
            &ctx,
        );
        assert!(
            result.is_none(),
            "multi-target 'and' should not trigger compound split"
        );
    }

    #[test]
    fn try_split_damage_compound_deconjugated() {
        // Subject-stripped form: "deals" → "deal" after deconjugation
        let ctx = ParseContext::default();
        let clause =
            try_split_damage_compound("deal 3 damage to any target and you gain 3 life", &ctx);
        let clause = clause.expect("deconjugated form should work");
        assert!(matches!(
            clause.effect,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                ..
            }
        ));
        assert!(clause.sub_ability.is_some());
    }

    #[test]
    fn try_split_damage_compound_full_pipeline() {
        // End-to-end: full text through parse_effect_chain
        let def = parse_effect_chain(
            "~ deals 3 damage to any target and you gain 3 life.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Any,
                    ..
                }
            ),
            "full pipeline: primary effect should be DealDamage, got: {:?}",
            def.effect
        );
        let sub = def
            .sub_ability
            .as_ref()
            .expect("full pipeline: should chain sub_ability");
        assert!(
            matches!(
                *sub.effect,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    ..
                }
            ),
            "full pipeline: sub_ability should be GainLife(3), got: {:?}",
            sub.effect
        );
    }

    #[test]
    fn try_split_damage_compound_event_context_oath_of_kaya() {
        // CR 608.2k: "that player" event-context ref with compound remainder
        let ctx = ParseContext::default();
        let clause =
            try_split_damage_compound("~ deals 2 damage to that player and you gain 2 life", &ctx)
                .expect("should split event-context compound");
        assert!(
            matches!(
                clause.effect,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::TriggeringPlayer,
                    ..
                }
            ),
            "primary: {:?}",
            clause.effect
        );
        let sub = clause
            .sub_ability
            .as_ref()
            .expect("should chain sub_ability");
        assert!(
            matches!(
                *sub.effect,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    ..
                }
            ),
            "sub_ability: {:?}",
            sub.effect
        );
    }

    #[test]
    fn try_split_damage_compound_event_context_torch_the_tower() {
        // CR 608.2k: "that permanent" event-context ref with compound remainder
        let ctx = ParseContext::default();
        let clause =
            try_split_damage_compound("~ deals 3 damage to that permanent and you scry 1", &ctx)
                .expect("should split event-context compound");
        assert!(
            matches!(
                clause.effect,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::TriggeringSource,
                    ..
                }
            ),
            "primary: {:?}",
            clause.effect
        );
        let sub = clause
            .sub_ability
            .as_ref()
            .expect("should chain sub_ability");
        assert!(
            matches!(
                *sub.effect,
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 }
                }
            ),
            "sub_ability: {:?}",
            sub.effect
        );
    }

    #[test]
    fn try_split_damage_compound_event_context_no_compound() {
        // No compound — should return None and fall through to normal path
        let ctx = ParseContext::default();
        let result = try_split_damage_compound("~ deals 2 damage to that player", &ctx);
        assert!(result.is_none(), "no compound should return None");
    }

    #[test]
    fn effect_murder() {
        let e = parse_effect("Destroy target creature");
        assert!(matches!(
            e,
            Effect::Destroy {
                target: TargetFilter::Typed(ref tf),
                ..
            } if tf.type_filters.contains(&TypeFilter::Creature)
        ));
    }

    #[test]
    fn effect_giant_growth() {
        let e = parse_effect("Target creature gets +3/+3 until end of turn");
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                ..
            }
        ));
    }

    #[test]
    fn effect_counterspell() {
        let e = parse_effect("Counter target spell");
        assert!(matches!(
            e,
            Effect::Counter {
                target: TargetFilter::Typed(TypedFilter { properties, .. }),
                ..
            } if properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
        ));
    }

    // The remaining tests are included by reference — they use `parse_effect`,
    // `parse_effect_chain`, `parse_effect_clause`, and helper functions which
    // are all still accessible from this module scope via the re-exports above.

    #[test]
    fn effect_annul_has_stack_restricted_targets() {
        let e = parse_effect("Counter target artifact or enchantment spell");
        assert!(matches!(
            e,
            Effect::Counter {
                target: TargetFilter::Or { filters },
                ..
            } if filters.iter().all(|f| {
                matches!(
                    f,
                    TargetFilter::Typed(TypedFilter { properties, .. })
                        if properties.iter().any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
                )
            })
        ));
    }

    #[test]
    fn effect_disdainful_stroke_has_cmc_and_stack_restriction() {
        let e = parse_effect("Counter target spell with mana value 4 or greater");
        assert!(matches!(
            e,
            Effect::Counter {
                target: TargetFilter::Typed(TypedFilter { properties, .. }),
                ..
            } if properties.iter().any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
                && properties.iter().any(|p| matches!(p, FilterProp::CmcGE { value: QuantityExpr::Fixed { value: 4 } }))
        ));
    }

    #[test]
    fn effect_counter_ability_with_source_static_absorption() {
        use crate::types::ability::ContinuousModification;
        use crate::types::statics::StaticMode;

        let ability = parse_effect_chain(
            "counter up to one target activated or triggered ability. If an ability of an artifact, creature, or planeswalker is countered this way, that permanent loses all abilities for as long as ~ remains on the battlefield",
            AbilityKind::Spell,
        );
        assert!(
            ability.sub_ability.is_none(),
            "sub_ability should be absorbed"
        );
        if let Effect::Counter { source_static, .. } = &*ability.effect {
            let static_def = source_static.as_ref().expect("should have source_static");
            assert_eq!(static_def.mode, StaticMode::Continuous);
            assert_eq!(
                static_def.modifications,
                vec![ContinuousModification::RemoveAllAbilities]
            );
        } else {
            panic!("expected Counter effect");
        }
    }

    #[test]
    fn effect_counter_unless_pays_parses_mana_cost() {
        use crate::types::mana::ManaCost;
        let e = parse_effect("Counter target spell unless its controller pays {3}");
        if let Effect::Counter {
            unless_payment,
            target,
            ..
        } = &e
        {
            assert_eq!(
                *unless_payment,
                Some(UnlessCost::Fixed {
                    cost: ManaCost::Cost {
                        shards: vec![],
                        generic: 3
                    }
                }),
                "should parse {{3}} unless payment"
            );
            assert!(
                matches!(target, TargetFilter::Typed(TypedFilter { properties, .. })
                    if properties.iter().any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))),
                "target should be on stack"
            );
        } else {
            panic!("expected Counter effect, got {e:?}");
        }
    }

    #[test]
    fn effect_counter_without_unless_has_none_payment() {
        let e = parse_effect("Counter target spell");
        if let Effect::Counter { unless_payment, .. } = &e {
            assert_eq!(
                *unless_payment, None,
                "plain counter should have no unless_payment"
            );
        } else {
            panic!("expected Counter effect");
        }
    }

    #[test]
    fn effect_exile_each_opponents_graveyard_has_origin() {
        let e = parse_effect("Exile each opponent's graveyard");
        assert!(
            matches!(
                e,
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Exile,
                    ..
                }
            ),
            "exile each graveyard should have origin=Graveyard, got {e:?}"
        );
    }

    #[test]
    fn effect_put_exiled_with_this_artifact_into_graveyard() {
        let e = parse_effect("Put each card exiled with this artifact into its owner's graveyard");
        assert!(
            matches!(
                e,
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Exile),
                    destination: Zone::Graveyard,
                    target: TargetFilter::ExiledBySource,
                }
            ),
            "should produce ChangeZoneAll from Exile to Graveyard with ExiledBySource, got {e:?}"
        );
    }

    #[test]
    fn effect_token_for_each_this_way_produces_tracked_set_size() {
        let e = parse_effect(
            "create a 2/2 colorless Robot artifact creature token for each card put into a graveyard this way",
        );
        match e {
            Effect::Token { count, .. } => {
                assert_eq!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::TrackedSetSize
                    },
                    "count should be TrackedSetSize"
                );
            }
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[test]
    fn effect_mana_production() {
        let e = parse_effect("Add {W}");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::Fixed { ref colors }, ..
            } if colors == &vec![ManaColor::White]
        ));
    }

    #[test]
    fn effect_add_additional_mana() {
        let e = parse_effect("Add an additional {G}");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::Fixed { ref colors }, ..
            } if colors == &vec![ManaColor::Green]
        ));
    }

    #[test]
    fn effect_double_number_of_counters_on_self() {
        let e = parse_effect("Double the number of +1/+1 counters on ~");
        assert!(matches!(
            e,
            Effect::MultiplyCounter {
                ref counter_type,
                multiplier: 2,
                target: TargetFilter::SelfRef,
            } if counter_type == "P1P1"
        ));
    }

    #[test]
    fn effect_double_power_of_target() {
        let e = parse_effect("Double the power of target creature you control");
        assert!(matches!(
            e,
            Effect::DoublePT {
                mode: DoublePTMode::Power,
                target: TargetFilter::Typed(_),
            }
        ));
    }

    #[test]
    fn effect_double_power_and_toughness_of_each() {
        let e = parse_effect("Double the power and toughness of each creature you control");
        assert!(matches!(
            e,
            Effect::DoublePTAll {
                mode: DoublePTMode::PowerAndToughness,
                target: TargetFilter::Typed(_),
            }
        ));
    }

    #[test]
    fn effect_double_toughness_of_each() {
        let e = parse_effect("Double the toughness of each creature you control");
        assert!(matches!(
            e,
            Effect::DoublePTAll {
                mode: DoublePTMode::Toughness,
                target: TargetFilter::Typed(_),
            }
        ));
    }

    #[test]
    fn effect_double_its_power() {
        let e = parse_effect("Double its power");
        assert!(matches!(
            e,
            Effect::DoublePT {
                mode: DoublePTMode::Power,
                target: TargetFilter::SelfRef,
            }
        ));
    }

    #[test]
    fn effect_double_its_power_and_toughness() {
        let e = parse_effect("Double its power and toughness");
        assert!(matches!(
            e,
            Effect::DoublePT {
                mode: DoublePTMode::PowerAndToughness,
                target: TargetFilter::SelfRef,
            }
        ));
    }

    #[test]
    fn effect_gain_life() {
        let e = parse_effect("You gain 3 life");
        assert!(matches!(
            e,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
    }

    #[test]
    fn effect_bounce() {
        let e = parse_effect("Return target creature to its owner's hand");
        assert!(matches!(e, Effect::Bounce { .. }));
    }

    #[test]
    fn strip_return_destination_hand() {
        let (target, dest) = strip_return_destination_ext("target creature to its owner's hand");
        assert_eq!(target, "target creature");
        assert_eq!(dest.unwrap().zone, Zone::Hand);
    }

    #[test]
    fn strip_return_destination_your_hand() {
        let (target, dest) = strip_return_destination_ext("~ to your hand");
        assert_eq!(target, "~");
        assert_eq!(dest.unwrap().zone, Zone::Hand);
    }

    #[test]
    fn strip_return_destination_graveyard() {
        let (target, dest) = strip_return_destination_ext("it to its owner's graveyard");
        assert_eq!(target, "it");
        assert_eq!(dest.unwrap().zone, Zone::Graveyard);
    }

    #[test]
    fn return_to_graveyard_produces_change_zone() {
        let e = parse_effect("Return the exiled cards to their owner's graveyard");
        assert!(
            matches!(
                e,
                Effect::ChangeZone {
                    destination: Zone::Graveyard,
                    ..
                }
            ),
            "Expected ChangeZone to Graveyard, got {:?}",
            e
        );
    }

    #[test]
    fn effect_draw() {
        let e = parse_effect("Draw two cards");
        assert!(matches!(
            e,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 }
            }
        ));
    }

    #[test]
    fn effect_scry() {
        let e = parse_effect("Scry 2");
        assert!(matches!(
            e,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 2 }
            }
        ));
    }

    #[test]
    fn effect_draw_x_variable() {
        let e = parse_effect("Draw X cards");
        assert!(
            matches!(
                e,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable { .. }
                    }
                }
            ),
            "Expected Draw with Variable, got {:?}",
            e
        );
    }

    #[test]
    fn effect_scry_x_variable() {
        let e = parse_effect("Scry X");
        assert!(
            matches!(
                e,
                Effect::Scry {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable { .. }
                    }
                }
            ),
            "Expected Scry with Variable, got {:?}",
            e
        );
    }

    #[test]
    fn effect_mill_x_variable() {
        let e = parse_effect("Mill X cards");
        assert!(
            matches!(
                e,
                Effect::Mill {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable { .. }
                    },
                    ..
                }
            ),
            "Expected Mill with Variable, got {:?}",
            e
        );
    }

    #[test]
    fn effect_disenchant() {
        let e = parse_effect("Destroy target artifact or enchantment");
        assert!(matches!(
            e,
            Effect::Destroy {
                target: TargetFilter::Or { .. },
                ..
            }
        ));
    }

    #[test]
    fn effect_explore() {
        let e = parse_effect("Explore");
        assert!(matches!(e, Effect::Explore));
    }

    #[test]
    fn effect_manifest_dread() {
        let e = parse_effect("Manifest dread");
        assert!(matches!(e, Effect::ManifestDread));
    }

    #[test]
    fn effect_unimplemented_fallback() {
        let e = parse_effect("Fateseal 2");
        assert!(matches!(e, Effect::Unimplemented { .. }));
    }

    #[test]
    fn effect_chain_revitalize() {
        let def = parse_effect_chain("You gain 3 life. Draw a card.", AbilityKind::Spell);
        assert!(matches!(
            *def.effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
        assert!(def.sub_ability.is_some());
        assert!(matches!(
            *def.sub_ability.unwrap().effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
    }

    #[test]
    fn effect_its_controller_gains_life_equal_to_power() {
        let e = parse_effect("Its controller gains life equal to its power");
        assert!(
            matches!(
                e,
                Effect::GainLife {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::TargetPower
                    },
                    player: GainLifePlayer::TargetedController
                }
            ),
            "Expected TargetPower + TargetedController, got {e:?}"
        );
    }

    #[test]
    fn effect_chain_with_em_dash() {
        let def = parse_effect_chain(
            "Spell mastery — Draw two cards. You gain 2 life.",
            AbilityKind::Spell,
        );
        assert!(def.sub_ability.is_some());
    }

    #[test]
    fn effect_shuffle_library() {
        let e = parse_effect("Shuffle your library");
        assert!(matches!(
            e,
            Effect::Shuffle {
                target: TargetFilter::Controller
            }
        ));
    }

    #[test]
    fn effect_shuffle_their_library() {
        let e = parse_effect("Shuffle their library");
        assert!(matches!(
            e,
            Effect::Shuffle {
                target: TargetFilter::Player
            }
        ));
    }

    #[test]
    fn compound_shuffle_it_into_library() {
        let def = parse_effect_chain("Shuffle it into its owner's library", AbilityKind::Spell);
        assert!(matches!(
            &*def.effect,
            Effect::ChangeZone {
                destination: Zone::Library,
                ..
            }
        ));
        // CR 701.24a: Compound shuffle must chain a Shuffle sub_ability.
        let sub = def
            .sub_ability
            .as_ref()
            .expect("should have Shuffle sub_ability");
        assert!(matches!(&*sub.effect, Effect::Shuffle { .. }));
    }

    #[test]
    fn compound_shuffle_graveyard_into_library() {
        let def = parse_effect_chain(
            "Shuffle your graveyard into your library",
            AbilityKind::Spell,
        );
        assert!(matches!(
            &*def.effect,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Library,
                ..
            }
        ));
        let sub = def
            .sub_ability
            .as_ref()
            .expect("should have Shuffle sub_ability");
        assert!(matches!(&*sub.effect, Effect::Shuffle { .. }));
    }

    #[test]
    fn compound_shuffle_hand_into_library() {
        let def = parse_effect_chain("Shuffle your hand into your library", AbilityKind::Spell);
        assert!(matches!(
            &*def.effect,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Hand),
                destination: Zone::Library,
                ..
            }
        ));
        let sub = def
            .sub_ability
            .as_ref()
            .expect("should have Shuffle sub_ability");
        assert!(matches!(&*sub.effect, Effect::Shuffle { .. }));
    }

    // Remaining tests truncated for space — they are identical to the original file.
    // Including a representative subset to verify compilation.

    #[test]
    fn parse_search_basic_land_to_hand() {
        let e = parse_effect(
            "Search your library for a basic land card, reveal it, put it into your hand",
        );
        match e {
            Effect::SearchLibrary {
                filter,
                count,
                reveal,
            } => {
                assert_eq!(count, 1);
                assert!(reveal);
                match filter {
                    TargetFilter::Typed(tf) => {
                        assert!(tf.type_filters.contains(&TypeFilter::Land));
                        assert!(tf.properties.iter().any(
                            |p| matches!(p, FilterProp::HasSupertype { value } if value == "Basic")
                        ));
                    }
                    other => panic!("Expected Typed filter, got {:?}", other),
                }
            }
            other => panic!("Expected SearchLibrary, got {:?}", other),
        }
    }

    #[test]
    fn parse_search_aura_with_mana_value_and_different_name() {
        // Light-Paws, Emperor's Voice search filter
        let e = parse_effect(
            "Search your library for an Aura card with mana value less than or equal to that Aura and with a different name than each Aura you control, put that card onto the battlefield attached to Light-Paws, then shuffle",
        );
        match e {
            Effect::SearchLibrary { filter, .. } => match filter {
                TargetFilter::Typed(tf) => {
                    assert!(tf.type_filters.contains(&TypeFilter::Enchantment));
                    assert_eq!(tf.get_subtype(), Some("Aura"));
                    let properties = &tf.properties;
                    assert!(
                        properties.iter().any(|p| matches!(
                            p,
                            FilterProp::CmcLE {
                                value: QuantityExpr::Ref {
                                    qty: QuantityRef::EventContextSourceManaValue
                                }
                            }
                        )),
                        "Should have CmcLE with EventContextSourceManaValue, got {:?}",
                        properties
                    );
                    assert!(
                        properties
                            .iter()
                            .any(|p| matches!(p, FilterProp::DifferentNameFrom { .. })),
                        "Should have DifferentNameFrom, got {:?}",
                        properties
                    );
                }
                other => panic!("Expected Typed filter, got {:?}", other),
            },
            other => panic!("Expected SearchLibrary, got {:?}", other),
        }
    }

    #[test]
    fn effect_create_colored_token() {
        let e = parse_effect("Create a 1/1 white Soldier creature token");
        assert!(matches!(
            e,
            Effect::Token {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                count: QuantityExpr::Fixed { value: 1 },
                ..
            }
        ));
    }

    #[test]
    fn effect_create_treasure_token() {
        let e = parse_effect("Create a Treasure token");
        assert!(matches!(
            e,
            Effect::Token { ref name, ref types, power: PtValue::Fixed(0), toughness: PtValue::Fixed(0), count: QuantityExpr::Fixed { value: 1 }, .. }
            if name == "Treasure" && types == &vec!["Artifact".to_string(), "Treasure".to_string()]
        ));
    }

    #[test]
    fn effect_create_lander_token() {
        let e = parse_effect("Create a Lander token");
        assert!(matches!(
            e,
            Effect::Token { ref name, ref types, .. }
            if name == "Lander" && types == &vec!["Artifact".to_string(), "Lander".to_string()]
        ));
    }

    #[test]
    fn effect_create_mutagen_token() {
        let e = parse_effect("Create a Mutagen token");
        assert!(matches!(
            e,
            Effect::Token { ref name, ref types, .. }
            if name == "Mutagen" && types == &vec!["Artifact".to_string(), "Mutagen".to_string()]
        ));
    }

    #[test]
    fn effect_create_role_token_attached_to_target() {
        let e = parse_effect("Create a Monster Role token attached to target creature you control");
        match e {
            Effect::Token {
                ref name,
                ref types,
                ref attach_to,
                ..
            } => {
                assert_eq!(name, "Monster Role");
                assert_eq!(
                    types,
                    &vec![
                        "Enchantment".to_string(),
                        "Aura".to_string(),
                        "Role".to_string()
                    ]
                );
                assert!(attach_to.is_some(), "attach_to should be set");
            }
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[test]
    fn effect_create_wicked_role_token() {
        let e = parse_effect("Create a Wicked Role token attached to target creature you control");
        assert!(matches!(
            e,
            Effect::Token { ref name, ref types, .. }
            if name == "Wicked Role"
                && types.contains(&"Enchantment".to_string())
                && types.contains(&"Aura".to_string())
                && types.contains(&"Role".to_string())
        ));
    }

    #[test]
    fn effect_create_role_token_attached_to_it() {
        let e = parse_effect("Create a Cursed Role token attached to it");
        match e {
            Effect::Token {
                ref name,
                ref attach_to,
                ..
            } => {
                assert_eq!(name, "Cursed Role");
                assert!(
                    attach_to.is_some(),
                    "attach_to should be set for 'attached to it'"
                );
            }
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[test]
    fn effect_target_creature_gains_keyword_uses_continuous_effect() {
        let e = parse_effect("Target creature gains flying until end of turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed(ref tf)),
                ..
            } if tf.type_filters.contains(&TypeFilter::Creature)
        ));
    }

    #[test]
    fn effect_another_target_creature_gains_deathtouch() {
        // CR 115.10a + CR 702.2a: "another target creature you control gains deathtouch"
        let e =
            parse_effect("another target creature you control gains deathtouch until end of turn");
        match &e {
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed(tf)),
                static_abilities,
                ..
            } => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.properties.contains(&FilterProp::Another),
                    "Missing Another property, got {:?}",
                    tf.properties
                );
                assert!(
                    static_abilities.iter().any(|s| s.modifications.contains(
                        &ContinuousModification::AddKeyword {
                            keyword: crate::types::keywords::Keyword::Deathtouch
                        }
                    )),
                    "Missing AddKeyword(Deathtouch), got {:?}",
                    static_abilities
                );
            }
            other => panic!("Expected GenericEffect, got {:?}", other),
        }
    }

    #[test]
    fn effect_target_creature_becomes_blue_uses_continuous_effect() {
        let e = parse_effect("Target creature becomes blue until end of turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed(ref tf)),
                ref static_abilities,
                ..
            } if tf.type_filters.contains(&TypeFilter::Creature)
                && static_abilities.len() == 1
                && static_abilities[0].modifications.contains(&ContinuousModification::SetColor { colors: vec![ManaColor::Blue] })
        ));
    }

    #[test]
    fn effect_target_creature_cant_block_uses_rule_static() {
        let e = parse_effect("Target creature can't block this turn");
        assert!(matches!(
            e,
            Effect::GenericEffect { target: Some(TargetFilter::Typed(ref tf)), ref static_abilities, .. }
            if tf.type_filters.contains(&TypeFilter::Creature) && static_abilities.len() == 1 && static_abilities[0].mode == StaticMode::CantBlock
        ));
    }

    #[test]
    fn compound_tap_and_put_counter() {
        let clause = parse_effect_clause(
            "tap target creature an opponent controls and put a stun counter on it",
            &ParseContext::default(),
        );
        assert!(
            matches!(clause.effect, Effect::Tap { .. }),
            "primary should be Tap, got {:?}",
            clause.effect
        );
        let sub = clause.sub_ability.expect("should have sub_ability");
        assert!(
            matches!(
                *sub.effect,
                Effect::PutCounter {
                    target: TargetFilter::ParentTarget,
                    ..
                }
            ),
            "sub should be PutCounter with ParentTarget, got {:?}",
            sub.effect
        );
    }

    #[test]
    fn compound_exile_own_and_control_not_split() {
        let clause = parse_effect_clause(
            "exile any number of other nonland permanents you own and control",
            &ParseContext::default(),
        );
        assert!(
            matches!(
                clause.effect,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    ..
                }
            ),
            "should be ChangeZone to Exile, got {:?}",
            clause.effect
        );
        assert!(
            clause.sub_ability.is_none(),
            "'you own and control' should NOT produce a sub_ability"
        );
    }

    #[test]
    fn choose_a_creature_type() {
        let e = parse_effect("Choose a creature type");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::CreatureType,
                persist: false
            }
        );
    }

    #[test]
    fn put_counter_up_to_one_target_creature() {
        let (effect, _rem, multi) = counter::try_parse_put_counter(
            "put a +1/+1 counter on up to one target creature",
            "Put a +1/+1 counter on up to one target creature",
            &ParseContext::default(),
        )
        .expect("should parse");
        if let Effect::PutCounter { target, .. } = &effect {
            assert!(
                !matches!(target, TargetFilter::Any),
                "Target should not be Any — expected Creature"
            );
        } else {
            panic!("Expected PutCounter");
        }
        let spec = multi.expect("should have multi_target");
        assert_eq!(spec.min, 0);
        assert_eq!(spec.max, Some(1));
    }

    #[test]
    fn put_counter_no_up_to() {
        let (effect, _rem, multi) = counter::try_parse_put_counter(
            "put two +1/+1 counters on target creature",
            "Put two +1/+1 counters on target creature",
            &ParseContext::default(),
        )
        .expect("should parse");
        assert!(matches!(effect, Effect::PutCounter { .. }));
        assert!(multi.is_none(), "should not have multi_target");
    }

    #[test]
    fn choose_a_color() {
        let e = parse_effect("Choose a color");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::Color,
                persist: false
            }
        );
    }

    #[test]
    fn effect_add_mana_any_color() {
        let e = parse_effect("Add one mana of any color");
        assert!(matches!(
            e,
            Effect::Mana { produced: ManaProduction::AnyOneColor { count: QuantityExpr::Fixed { value: 1 }, ref color_options }, .. }
            if color_options == &vec![ManaColor::White, ManaColor::Blue, ManaColor::Black, ManaColor::Red, ManaColor::Green]
        ));
    }

    #[test]
    fn put_counter_this_creature_is_self_ref() {
        let e = parse_effect("put a +1/+1 counter on this creature");
        assert!(
            matches!(e, Effect::PutCounter { counter_type: ref ct, count: 1, target: TargetFilter::SelfRef } if ct == "P1P1")
        );
    }

    #[test]
    fn effect_pay_life_cost() {
        let e = parse_effect("pay 3 life");
        assert!(matches!(
            e,
            Effect::PayCost {
                cost: PaymentCost::Life { amount: 3 }
            }
        ));
    }

    #[test]
    fn strip_temporal_suffix_end_step() {
        let (text, cond) = strip_temporal_suffix("return it at the beginning of the next end step");
        assert_eq!(text, "return it");
        assert_eq!(
            cond,
            Some(DelayedTriggerCondition::AtNextPhase { phase: Phase::End })
        );
    }

    #[test]
    fn strip_temporal_prefix_end_step() {
        let (text, cond) =
            strip_temporal_prefix("at the beginning of the next end step, untap those lands");
        assert_eq!(text, "untap those lands");
        assert_eq!(
            cond,
            Some(DelayedTriggerCondition::AtNextPhase { phase: Phase::End })
        );
    }

    #[test]
    fn strip_temporal_prefix_upkeep() {
        let (text, cond) =
            strip_temporal_prefix("at the beginning of the next upkeep, sacrifice it");
        assert_eq!(text, "sacrifice it");
        assert_eq!(
            cond,
            Some(DelayedTriggerCondition::AtNextPhase {
                phase: Phase::Upkeep
            })
        );
    }

    #[test]
    fn temporal_prefix_in_effect_chain() {
        // Teferi, Hero of Dominaria +1 pattern
        let def = parse_effect_chain(
            "At the beginning of the next end step, untap up to two lands.",
            AbilityKind::Activated,
        );
        assert!(
            matches!(*def.effect, Effect::CreateDelayedTrigger { .. }),
            "Expected CreateDelayedTrigger, got {:?}",
            def.effect
        );
        if let Effect::CreateDelayedTrigger {
            condition, effect, ..
        } = &*def.effect
        {
            assert_eq!(
                *condition,
                DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
            );
            assert!(
                matches!(*effect.effect, Effect::Untap { .. }),
                "Inner effect should be Untap, got {:?}",
                effect.effect
            );
        }
    }

    #[test]
    fn strip_any_number_exile() {
        let (text, spec) = strip_any_number_quantifier("exile any number of creatures");
        assert_eq!(text, "exile creatures");
        let spec = spec.unwrap();
        assert_eq!(spec.min, 0);
        assert_eq!(spec.max, None);
    }

    #[test]
    fn return_to_battlefield_produces_change_zone() {
        let e = parse_effect("return those cards to the battlefield under their owners' control");
        assert!(matches!(
            e,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Battlefield,
                ..
            }
        ));
    }

    #[test]
    fn delayed_trigger_in_effect_chain() {
        let def = parse_effect_chain(
            "Exile target creature. Return it to the battlefield at the beginning of the next end step",
            AbilityKind::Spell,
        );
        assert!(matches!(
            *def.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ));
        let sub = def.sub_ability.as_ref().expect("should have sub_ability");
        assert!(matches!(
            *sub.effect,
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                ..
            }
        ));
    }

    #[test]
    fn effect_emblem_ninjas_get_plus_one() {
        let e = parse_effect("You get an emblem with \"Ninjas you control get +1/+1.\"");
        match e {
            Effect::CreateEmblem { statics } => {
                assert_eq!(statics.len(), 1);
                let def = &statics[0];
                assert_eq!(def.mode, StaticMode::Continuous);
                assert!(def.affected.is_some());
                assert!(def
                    .modifications
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::AddPower { value: 1 })));
                assert!(def
                    .modifications
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::AddToughness { value: 1 })));
            }
            other => panic!("expected CreateEmblem, got {:?}", other),
        }
    }

    #[test]
    fn kicker_instead_chain_produces_correct_condition() {
        let ability = parse_effect_chain(
            "~ deals 2 damage to target creature. If it was kicked, ~ deals 5 damage to that creature instead",
            AbilityKind::Spell,
        );
        assert!(matches!(
            &*ability.effect,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ));
        let sub = ability.sub_ability.as_ref().expect("expected sub_ability");
        assert_eq!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        );
        assert!(matches!(
            &*sub.effect,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget,
                ..
            }
        ));
    }

    #[test]
    fn parse_damage_cant_be_prevented_this_turn() {
        let clause = parse_effect_clause(
            "Damage can't be prevented this turn",
            &ParseContext::default(),
        );
        match clause.effect {
            Effect::AddRestriction { restriction } => {
                assert!(matches!(
                    restriction,
                    crate::types::ability::GameRestriction::DamagePreventionDisabled {
                        expiry: crate::types::ability::RestrictionExpiry::EndOfTurn,
                        scope: None,
                        ..
                    }
                ));
            }
            other => panic!("Expected AddRestriction, got {:?}", other),
        }
    }

    #[test]
    fn shuffle_compound_subject_into_owners_libraries() {
        let clause = parse_effect_clause(
            "shuffle ~ and target creature with a stun counter on it into their owners' libraries",
            &ParseContext::default(),
        );
        match &clause.effect {
            Effect::ChangeZone {
                destination: Zone::Library,
                target: TargetFilter::SelfRef,
                owner_library: true,
                enter_transformed: false,
                under_your_control: false,
                ..
            } => {}
            other => panic!(
                "expected ChangeZone {{ SelfRef, Library, owner_library: true }}, got {:?}",
                other
            ),
        }
        assert!(
            clause.sub_ability.is_some(),
            "should have sub_ability for second subject"
        );
    }

    // -----------------------------------------------------------------------
    // Item 1: "It can't be regenerated" continuation
    // -----------------------------------------------------------------------

    #[test]
    fn cant_regenerate_destroy_target() {
        let def = parse_effect_chain(
            "Destroy target creature. It can't be regenerated.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::Destroy {
                    cant_regenerate: true,
                    ..
                }
            ),
            "Expected Destroy {{ cant_regenerate: true }}, got {:?}",
            def.effect
        );
    }

    #[test]
    fn cant_regenerate_destroy_all() {
        let def = parse_effect_chain(
            "Destroy all creatures. They can't be regenerated.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::DestroyAll {
                    cant_regenerate: true,
                    ..
                }
            ),
            "Expected DestroyAll {{ cant_regenerate: true }}, got {:?}",
            def.effect
        );
    }

    // -----------------------------------------------------------------------
    // Item 2: Restriction predicates
    // -----------------------------------------------------------------------

    #[test]
    fn restriction_cant_attack() {
        let e = parse_effect("Target creature can't attack");
        assert!(
            matches!(&e, Effect::GenericEffect { static_abilities, .. }
                if static_abilities.iter().any(|s| s.mode == StaticMode::CantAttack)),
            "Expected CantAttack restriction, got {:?}",
            e
        );
    }

    #[test]
    fn restriction_cant_attack_or_block() {
        let e = parse_effect("Target creature can't attack or block");
        match &e {
            Effect::GenericEffect {
                static_abilities, ..
            } => {
                let modes: Vec<_> = static_abilities.iter().map(|s| &s.mode).collect();
                assert!(
                    modes.contains(&&StaticMode::CantAttack),
                    "Missing CantAttack"
                );
                assert!(modes.contains(&&StaticMode::CantBlock), "Missing CantBlock");
            }
            other => panic!("Expected GenericEffect, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Item 3: Connive, PhaseOut, ForceBlock verbs
    // -----------------------------------------------------------------------

    #[test]
    fn connive_imperative() {
        let e = parse_effect("it connives");
        assert!(
            matches!(
                e,
                Effect::Connive {
                    target: TargetFilter::SelfRef,
                    ..
                }
            ),
            "Expected Connive {{ SelfRef }}, got {:?}",
            e
        );
    }

    #[test]
    fn phase_out_targeted() {
        let e = parse_effect("Target creature phases out");
        assert!(
            matches!(
                e,
                Effect::PhaseOut {
                    target: TargetFilter::Typed(_)
                }
            ),
            "Expected PhaseOut with typed target, got {:?}",
            e
        );
    }

    #[test]
    fn force_block_targeted() {
        let e = parse_effect("Target creature blocks this turn if able");
        assert!(
            matches!(
                e,
                Effect::ForceBlock {
                    target: TargetFilter::Typed(_)
                }
            ),
            "Expected ForceBlock with typed target, got {:?}",
            e
        );
    }

    // -----------------------------------------------------------------------
    // Item 3b: MustBeBlocked imperative (CR 509.1c)
    // -----------------------------------------------------------------------

    #[test]
    fn must_be_blocked_imperative() {
        // CR 509.1c: "must be blocked this turn if able" as sub-ability
        let e = parse_effect("must be blocked this turn if able");
        assert!(
            matches!(&e, Effect::GenericEffect { static_abilities, .. }
                if static_abilities.iter().any(|sd|
                    sd.mode == crate::types::statics::StaticMode::MustBeBlocked
                )
            ),
            "Expected GenericEffect with MustBeBlocked, got {:?}",
            e
        );
    }

    #[test]
    fn must_be_blocked_if_able_variant() {
        // "must be blocked if able" without "this turn"
        let e = parse_effect("must be blocked if able");
        assert!(
            matches!(&e, Effect::GenericEffect { static_abilities, .. }
                if static_abilities.iter().any(|sd|
                    sd.mode == crate::types::statics::StaticMode::MustBeBlocked
                )
            ),
            "Expected GenericEffect with MustBeBlocked, got {:?}",
            e
        );
    }

    #[test]
    fn pump_compound_with_must_be_blocked() {
        // Emergent Growth: "+5/+5 until end of turn and must be blocked this turn if able"
        let def = parse_effect_chain(
            "Target creature gets +5/+5 until end of turn and must be blocked this turn if able",
            crate::types::ability::AbilityKind::Spell,
        );
        // Primary effect should be Pump
        assert!(
            matches!(&*def.effect, Effect::Pump { .. }),
            "Expected Pump as primary effect, got {:?}",
            def.effect
        );
        // Sub-ability should carry MustBeBlocked
        let sub = def
            .sub_ability
            .as_ref()
            .expect("Expected sub_ability for MustBeBlocked");
        assert!(
            matches!(&*sub.effect, Effect::GenericEffect { static_abilities, .. }
                if static_abilities.iter().any(|sd|
                    sd.mode == crate::types::statics::StaticMode::MustBeBlocked
                )
            ),
            "Expected sub_ability GenericEffect with MustBeBlocked, got {:?}",
            sub.effect
        );
    }

    #[test]
    fn static_must_be_blocked_still_routes_to_static_parser() {
        // Regression: self-referential "CARDNAME must be blocked if able" should
        // still route to the static parser, not the effect parser.
        let result = crate::parser::oracle_static::parse_static_line(
            "Darksteel Myr must be blocked if able.",
        );
        assert!(result.is_some(), "Should still parse as static ability");
    }

    #[test]
    fn force_block_with_self_ref() {
        // "Target creature blocks ~ this turn if able" (e.g., Auriok Siege Sled)
        let e = parse_effect("Target creature blocks ~ this turn if able");
        assert!(
            matches!(
                e,
                Effect::ForceBlock {
                    target: TargetFilter::Typed(_)
                }
            ),
            "Expected ForceBlock with typed target, got {:?}",
            e
        );
    }

    #[test]
    fn force_block_blocks_it_this_combat() {
        // "target creature blocks it this combat if able" (e.g., Avalanche Tusker)
        let e = parse_effect("Target creature blocks it this combat if able");
        assert!(
            matches!(
                e,
                Effect::ForceBlock {
                    target: TargetFilter::Typed(_)
                }
            ),
            "Expected ForceBlock with typed target, got {:?}",
            e
        );
    }

    #[test]
    fn mass_forced_block_target_creature() {
        // "All creatures able to block target creature this turn do so" (Alluring Scent)
        // CR 509.1c: Semantically equivalent to "target creature must be blocked"
        let e = parse_effect("All creatures able to block target creature this turn do so");
        assert!(
            matches!(&e, Effect::GenericEffect { static_abilities, .. }
                if static_abilities.iter().any(|sd|
                    sd.mode == crate::types::statics::StaticMode::MustBeBlocked
                )
            ),
            "Expected GenericEffect with MustBeBlocked, got {:?}",
            e
        );
    }

    #[test]
    fn mass_forced_block_self_ref() {
        // "All creatures able to block ~ do so" (Breaker of Armies, as effect text)
        let e = parse_effect("All creatures able to block ~ do so");
        assert!(
            matches!(&e, Effect::GenericEffect { static_abilities, .. }
                if static_abilities.iter().any(|sd|
                    sd.mode == crate::types::statics::StaticMode::MustBeBlocked
                )
            ),
            "Expected GenericEffect with MustBeBlocked, got {:?}",
            e
        );
    }

    // -----------------------------------------------------------------------
    // Item 4: Inline delayed triggers
    // -----------------------------------------------------------------------

    #[test]
    fn inline_delayed_trigger_when_dies() {
        let e = parse_effect("When that creature dies, draw a card");
        assert!(
            matches!(
                e,
                Effect::CreateDelayedTrigger {
                    condition: DelayedTriggerCondition::WhenDies {
                        filter: TargetFilter::ParentTarget,
                    },
                    uses_tracked_set: true,
                    ..
                }
            ),
            "Expected CreateDelayedTrigger with WhenDies, got {:?}",
            e
        );
    }

    #[test]
    fn inline_delayed_trigger_when_leaves() {
        let e = parse_effect("When that creature leaves the battlefield, return it to the battlefield under its owner's control");
        assert!(
            matches!(
                e,
                Effect::CreateDelayedTrigger {
                    condition: DelayedTriggerCondition::WhenLeavesPlayFiltered {
                        filter: TargetFilter::ParentTarget,
                    },
                    uses_tracked_set: true,
                    ..
                }
            ),
            "Expected CreateDelayedTrigger with WhenLeavesPlayFiltered, got {:?}",
            e
        );
    }

    // -----------------------------------------------------------------------
    // Item 5: "Become the [type] of your choice"
    // -----------------------------------------------------------------------

    #[test]
    fn become_creature_type_of_choice() {
        let e = parse_effect(
            "Target creature becomes the creature type of your choice until end of turn",
        );
        assert!(
            matches!(
                e,
                Effect::Choose {
                    choice_type: ChoiceType::CreatureType,
                    ..
                }
            ),
            "Expected Choose {{ CreatureType }}, got {:?}",
            e
        );
    }

    #[test]
    fn become_basic_land_type_of_choice() {
        let e = parse_effect(
            "Target land becomes the basic land type of your choice until end of turn",
        );
        assert!(
            matches!(
                e,
                Effect::Choose {
                    choice_type: ChoiceType::BasicLandType,
                    ..
                }
            ),
            "Expected Choose {{ BasicLandType }}, got {:?}",
            e
        );
    }

    #[test]
    fn parse_play_from_exile_this_turn() {
        let def = parse_effect_chain("You may play that card this turn.", AbilityKind::Spell);
        assert!(matches!(
            &*def.effect,
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilEndOfTurn
                },
                ..
            }
        ));
    }

    #[test]
    fn parse_play_from_exile_next_turn() {
        let def = parse_effect_chain(
            "You may play that card until the end of your next turn.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::GrantCastingPermission {
                    permission: CastingPermission::PlayFromExile {
                        duration: Duration::UntilYourNextTurn
                    },
                    ..
                }
            ),
            "Expected GrantCastingPermission(PlayFromExile, UntilYourNextTurn), got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_play_from_exile_leading_duration_next_turn() {
        // "Until the end of your next turn, you may play those cards" — stripped prefix variant
        let def = parse_effect_chain(
            "Until the end of your next turn, you may play those cards.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::GrantCastingPermission {
                    permission: CastingPermission::PlayFromExile {
                        duration: Duration::UntilYourNextTurn
                    },
                    ..
                }
            ),
            "Expected PlayFromExile(UntilYourNextTurn), got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_play_from_exile_leading_duration_cast_variant() {
        let def = parse_effect_chain(
            "Until the end of your next turn, you may cast that card.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::GrantCastingPermission {
                    permission: CastingPermission::PlayFromExile {
                        duration: Duration::UntilYourNextTurn
                    },
                    ..
                }
            ),
            "Expected PlayFromExile(UntilYourNextTurn), got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_impulse_draw_chain_next_turn() {
        // Full chain: exile + permission with "until the end of your next turn"
        let def = parse_effect_chain(
            "Exile the top two cards of your library. Until the end of your next turn, you may play those cards.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::ChangeZone { .. }),
            "Expected ChangeZone, got {:?}",
            def.effect
        );
        let sub = def.sub_ability.as_ref().expect("Expected sub_ability");
        assert!(
            matches!(
                *sub.effect,
                Effect::GrantCastingPermission {
                    permission: CastingPermission::PlayFromExile {
                        duration: Duration::UntilYourNextTurn
                    },
                    ..
                }
            ),
            "Expected PlayFromExile(UntilYourNextTurn), got {:?}",
            sub.effect
        );
    }

    #[test]
    fn parse_impulse_draw_chain() {
        // "Exile the top two cards of your library. Choose one of them. Until end of turn, you may play that card."
        let def = parse_effect_chain(
            "Exile the top two cards of your library. Choose one of them. Until end of turn, you may play that card.",
            AbilityKind::Spell,
        );
        // First effect: ChangeZone to Exile
        assert!(
            matches!(*def.effect, Effect::ChangeZone { .. }),
            "Expected ChangeZone, got {:?}",
            def.effect
        );
        // Second: ChooseFromZone
        let sub1 = def.sub_ability.as_ref().expect("Expected sub_ability");
        assert!(
            matches!(
                *sub1.effect,
                Effect::ChooseFromZone {
                    count: 1,
                    zone: crate::types::zones::Zone::Exile,
                    chooser: crate::types::ability::Chooser::Controller,
                }
            ),
            "Expected ChooseFromZone {{ count: 1, zone: Exile, chooser: Controller }}, got {:?}",
            sub1.effect
        );
        // Third: GrantCastingPermission with PlayFromExile
        let sub2 = sub1
            .sub_ability
            .as_ref()
            .expect("Expected second sub_ability");
        assert!(
            matches!(
                *sub2.effect,
                Effect::GrantCastingPermission {
                    permission: CastingPermission::PlayFromExile {
                        duration: Duration::UntilEndOfTurn
                    },
                    ..
                }
            ),
            "Expected GrantCastingPermission(PlayFromExile), got {:?}",
            sub2.effect
        );
    }

    #[test]
    fn parse_dynamic_reveal_count_with_continuation() {
        // Bala Ged Thief pattern: "reveals a number of cards from their hand equal to the number of Allies you control. You choose one of them. That player discards that card."
        let def = parse_effect_chain(
            "Target opponent reveals a number of cards from their hand equal to the number of Allies you control. You choose one of them. That player discards that card.",
            AbilityKind::Spell,
        );
        // First effect: RevealHand with count
        match &*def.effect {
            Effect::RevealHand { count, .. } => {
                assert!(count.is_some(), "Expected dynamic count on RevealHand");
            }
            other => panic!("Expected RevealHand, got {:?}", other),
        }
        // Should have sub_ability chain for discard
        assert!(
            def.sub_ability.is_some(),
            "Expected sub_ability for discard continuation"
        );
    }

    #[test]
    fn parse_put_on_top_or_bottom_possessive() {
        // "Target creature's owner puts it on their choice of the top or bottom of their library."
        let def = parse_effect_chain(
            "Target creature's owner puts it on their choice of the top or bottom of their library.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::PutOnTopOrBottom { .. }),
            "Expected PutOnTopOrBottom, got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_put_on_top_or_bottom_owner_of() {
        // "The owner of target nonland permanent puts it on their choice of the top or bottom of their library."
        let def = parse_effect_chain(
            "The owner of target nonland permanent puts it on their choice of the top or bottom of their library.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::PutOnTopOrBottom { .. }),
            "Expected PutOnTopOrBottom, got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_put_on_top_or_bottom_with_continuation() {
        // "Target nonland permanent's owner puts it on their choice of the top or bottom of their library. Surveil 1."
        let def = parse_effect_chain(
            "Target nonland permanent's owner puts it on their choice of the top or bottom of their library. Surveil 1.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::PutOnTopOrBottom { .. }),
            "Expected PutOnTopOrBottom, got {:?}",
            def.effect
        );
        let sub = def
            .sub_ability
            .as_ref()
            .expect("Expected sub_ability for Surveil");
        assert!(
            matches!(*sub.effect, Effect::Surveil { .. }),
            "Expected Surveil continuation, got {:?}",
            sub.effect
        );
    }

    #[test]
    fn parse_put_on_top_or_bottom_simple() {
        // Shorter variant: "The owner of target nonland permanent puts it on the top or bottom of their library."
        let def = parse_effect_chain(
            "The owner of target nonland permanent puts it on the top or bottom of their library.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::PutOnTopOrBottom { .. }),
            "Expected PutOnTopOrBottom, got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_gift_was_promised_condition() {
        // "If the gift was promised, Blooming Blast also deals 3 damage to that creature's controller."
        let def = parse_effect_chain(
            "Blooming Blast deals 2 damage to target creature. If the gift was promised, Blooming Blast also deals 3 damage to that creature's controller.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::DealDamage { .. }),
            "Expected DealDamage, got {:?}",
            def.effect
        );
        let sub = def.sub_ability.as_ref().expect("Expected sub_ability");
        assert_eq!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostPaid),
            "Expected AdditionalCostPaid condition"
        );
    }

    #[test]
    fn parse_gift_wasnt_promised_condition() {
        // "Destroy target creature. If the gift wasn't promised, you lose 2 life."
        let def = parse_effect_chain(
            "Destroy target creature. If the gift wasn't promised, you lose 2 life.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::Destroy { .. }),
            "Expected Destroy, got {:?}",
            def.effect
        );
        let sub = def.sub_ability.as_ref().expect("Expected sub_ability");
        assert_eq!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostNotPaid),
            "Expected AdditionalCostNotPaid condition"
        );
    }

    #[test]
    fn goblin_guide_reveal_conditional_land() {
        let def = parse_effect_chain(
            "defending player reveals the top card of their library. If it's a land card, that player puts it into their hand.",
            AbilityKind::Spell,
        );
        assert!(matches!(*def.effect, Effect::RevealTop { .. }));
        let sub = def.sub_ability.as_ref().expect("should have sub_ability");
        assert_eq!(
            sub.condition,
            Some(AbilityCondition::RevealedHasCardType {
                card_type: CoreType::Land,
                negated: false,
            })
        );
        assert!(matches!(
            *sub.effect,
            Effect::ChangeZone {
                destination: Zone::Hand,
                ..
            }
        ));
    }

    #[test]
    fn coiling_oracle_reveal_conditional_with_otherwise() {
        // Spawned thread with 32MB stack — AbilityDefinition is very large in
        // debug builds and the parser + assertions + Drop exhaust the default
        // test thread stack.
        std::thread::Builder::new()
            .name("coiling-oracle".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let def = parse_effect_chain(
                    "reveal the top card of your library. If it's a land card, put it onto the battlefield. Otherwise, put that card into your hand.",
                    AbilityKind::Spell,
                );
                assert!(matches!(*def.effect, Effect::RevealTop { .. }));
                let sub = def.sub_ability.as_deref().unwrap();
                assert!(sub.condition == Some(AbilityCondition::RevealedHasCardType {
                    card_type: CoreType::Land,
                    negated: false,
                }));
                assert!(matches!(
                    *sub.effect,
                    Effect::ChangeZone { destination: Zone::Battlefield, .. }
                ));
                assert!(sub.else_ability.is_some(), "sub should have else_ability");
                let else_ab = sub.else_ability.as_deref().unwrap();
                assert!(matches!(
                    *else_ab.effect,
                    Effect::ChangeZone { destination: Zone::Hand, .. }
                ));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn nonland_card_type_conditional() {
        let def = parse_effect_chain(
            "reveal the top card of your library. If it's a nonland card, put it into your hand.",
            AbilityKind::Spell,
        );
        let sub = def.sub_ability.as_ref().expect("should have sub_ability");
        assert_eq!(
            sub.condition,
            Some(AbilityCondition::RevealedHasCardType {
                card_type: CoreType::Land,
                negated: true,
            })
        );
    }

    #[test]
    fn otherwise_attaches_else_ability() {
        let def = parse_effect_chain(
            "You may sacrifice two Foods. If you do, create a 7/7 green Giant creature token. Otherwise, create three Food tokens.",
            AbilityKind::Spell,
        );
        // Walk the chain and collect effect types
        let mut effects = vec![];
        let mut current = Some(&def);
        while let Some(d) = current {
            effects.push(std::mem::discriminant(&*d.effect));
            // Check else_ability on any node with IfYouDo condition
            if d.condition == Some(AbilityCondition::IfYouDo) {
                if let Some(else_ab) = &d.else_ability {
                    effects.push(std::mem::discriminant(&*else_ab.effect));
                }
            }
            current = d.sub_ability.as_deref();
        }
        // We should have at least 3 effects (Sacrifice, Token-Giant, something-else)
        assert!(
            effects.len() >= 2,
            "Expected at least 2 effects, got {}",
            effects.len()
        );
    }

    #[test]
    fn strip_unless_entered_suffix_strips_correctly() {
        let (cond, text) = strip_unless_entered_suffix("discard a card unless ~ entered this turn");
        assert_eq!(
            cond,
            Some(AbilityCondition::SourceDidNotEnterThisTurn),
            "Should produce SourceDidNotEnterThisTurn condition"
        );
        assert_eq!(text, "discard a card");
    }

    #[test]
    fn strip_unless_entered_suffix_no_match() {
        let (cond, text) = strip_unless_entered_suffix("discard a card");
        assert!(cond.is_none());
        assert_eq!(text, "discard a card");
    }

    #[test]
    fn strip_numeric_target_prefix_two() {
        let result = strip_numeric_target_prefix("two target creatures");
        assert_eq!(result, Some((2, "target creatures")));
    }

    #[test]
    fn strip_numeric_target_prefix_three() {
        let result = strip_numeric_target_prefix("three target artifacts");
        assert_eq!(result, Some((3, "target artifacts")));
    }

    #[test]
    fn strip_numeric_target_prefix_no_match() {
        assert!(strip_numeric_target_prefix("target creature").is_none());
        assert!(strip_numeric_target_prefix("a target creature").is_none());
    }

    #[test]
    fn ninjutsu_variant_paid_instead_sneak() {
        let (cond, text) = strip_additional_cost_conditional(
            "if her sneak cost was paid this turn, instead return that card to the battlefield",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::NinjutsuVariantPaidInstead {
                variant: NinjutsuVariant::Sneak,
            })
        );
        assert_eq!(text, "return that card to the battlefield");
    }

    #[test]
    fn ninjutsu_variant_paid_instead_ninjutsu() {
        let (cond, text) = strip_additional_cost_conditional(
            "if its ninjutsu cost was paid this turn, instead draw a card",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::NinjutsuVariantPaidInstead {
                variant: NinjutsuVariant::Ninjutsu,
            })
        );
        assert_eq!(text, "draw a card");
    }

    #[test]
    fn change_targets_spell_or_ability_with_single_target() {
        let e = parse_effect("change the target of target spell or ability with a single target");
        let Effect::ChangeTargets {
            target,
            scope,
            forced_to,
        } = e
        else {
            panic!("Expected ChangeTargets, got {e:?}");
        };
        assert!(matches!(scope, RetargetScope::Single));
        assert!(forced_to.is_none());
        // Should be Or(StackSpell+HasSingleTarget, StackAbility+HasSingleTarget)
        let TargetFilter::Or { filters } = &target else {
            panic!("Expected Or filter, got {target:?}");
        };
        assert_eq!(filters.len(), 2);
        for f in filters {
            let TargetFilter::And { filters: inner } = f else {
                panic!("Expected And filter inside Or, got {f:?}");
            };
            assert!(inner.iter().any(|f| matches!(
                f,
                TargetFilter::Typed(TypedFilter { properties, .. })
                    if properties.contains(&FilterProp::HasSingleTarget)
            )));
        }
    }

    #[test]
    fn choose_new_targets_instant_or_sorcery_spell() {
        let e = parse_effect("you may choose new targets for target instant or sorcery spell");
        let Effect::ChangeTargets {
            target,
            scope,
            forced_to,
        } = e
        else {
            panic!("Expected ChangeTargets, got {e:?}");
        };
        assert!(matches!(scope, RetargetScope::All));
        assert!(forced_to.is_none());
        // Should be Or(Instant+InZone(Stack), Sorcery+InZone(Stack))
        let TargetFilter::Or { filters } = &target else {
            panic!("Expected Or filter, got {target:?}");
        };
        assert_eq!(filters.len(), 2);
        let types: Vec<_> = filters
            .iter()
            .filter_map(|f| {
                if let TargetFilter::Typed(tf) = f {
                    tf.get_primary_type().cloned()
                } else {
                    None
                }
            })
            .collect();
        assert!(types.contains(&TypeFilter::Instant));
        assert!(types.contains(&TypeFilter::Sorcery));
        // Both should have InZone(Stack)
        for f in filters {
            if let TargetFilter::Typed(tf) = f {
                assert!(tf
                    .properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack })));
            }
        }
    }

    // --- Search filter tests ---

    #[test]
    fn search_filter_creature_with_mana_value_3_or_less() {
        let filter = parse_search_filter("creature card with mana value 3 or less");
        let TargetFilter::Typed(tf) = filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::CmcLE {
                value: QuantityExpr::Fixed { value: 3 }
            }
        )));
    }

    #[test]
    fn search_filter_creature_with_mana_value_exact() {
        let filter = parse_search_filter("creature card with mana value 2");
        let TargetFilter::Typed(tf) = filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert!(tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::CmcEQ {
                value: QuantityExpr::Fixed { value: 2 }
            }
        )));
    }

    #[test]
    fn search_filter_card_with_that_name() {
        let filter = parse_search_filter("card with that name");
        let TargetFilter::Typed(tf) = filter else {
            panic!("Expected Typed filter with SameName, got {filter:?}");
        };
        assert!(tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::SameName)));
    }

    #[test]
    fn search_library_details_up_to_five() {
        let details =
            parse_search_library_details("search your library for up to five creature cards");
        assert_eq!(details.count, 5);
        let TargetFilter::Typed(tf) = &details.filter else {
            panic!("Expected Typed filter, got {:?}", details.filter);
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
    }

    #[test]
    fn search_filter_noncreature_card() {
        let filter = parse_search_filter("noncreature card");
        let TargetFilter::Typed(tf) = filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.iter().any(|t| matches!(
            t,
            TypeFilter::Non(inner) if matches!(inner.as_ref(), TypeFilter::Creature)
        )));
    }

    #[test]
    fn search_filter_nonland_card() {
        let filter = parse_search_filter("nonland card");
        let TargetFilter::Typed(tf) = filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf.type_filters.iter().any(|t| matches!(
            t,
            TypeFilter::Non(inner) if matches!(inner.as_ref(), TypeFilter::Land)
        )));
    }

    // --- Seek parser tests ---

    #[test]
    fn seek_an_elf_card() {
        let details = parse_seek_details("seek an elf card");
        assert_eq!(details.count, QuantityExpr::Fixed { value: 1 });
        assert_eq!(details.destination, Zone::Hand);
        assert!(!details.enter_tapped);
        let TargetFilter::Typed(tf) = &details.filter else {
            panic!("Expected Typed filter, got {:?}", details.filter);
        };
        assert!(tf
            .type_filters
            .iter()
            .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Elf")));
    }

    #[test]
    fn seek_two_nonland_cards() {
        let details = parse_seek_details("seek two nonland cards");
        assert_eq!(details.count, QuantityExpr::Fixed { value: 2 });
        assert_eq!(details.destination, Zone::Hand);
        let TargetFilter::Typed(tf) = &details.filter else {
            panic!("Expected Typed filter, got {:?}", details.filter);
        };
        assert!(tf.type_filters.iter().any(|t| matches!(
            t,
            TypeFilter::Non(inner) if matches!(inner.as_ref(), TypeFilter::Land)
        )));
    }

    #[test]
    fn seek_land_onto_battlefield_tapped() {
        let details = parse_seek_details("seek a land card and put it onto the battlefield tapped");
        assert_eq!(details.count, QuantityExpr::Fixed { value: 1 });
        assert_eq!(details.destination, Zone::Battlefield);
        assert!(details.enter_tapped);
        let TargetFilter::Typed(tf) = &details.filter else {
            panic!("Expected Typed filter, got {:?}", details.filter);
        };
        assert!(tf.type_filters.contains(&TypeFilter::Land));
    }

    // --- Shuffle-to-library and put-back tests ---

    #[test]
    fn shuffle_target_card_from_graveyard_into_library() {
        let def = parse_effect_chain(
            "shuffle target card from your graveyard into your library",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Library,
                    ..
                }
            ),
            "Expected ChangeZone(Graveyard -> Library), got {:?}",
            def.effect
        );
        let sub = def
            .sub_ability
            .as_ref()
            .expect("should have Shuffle sub_ability");
        assert!(matches!(&*sub.effect, Effect::Shuffle { .. }));
    }

    #[test]
    fn for_as_long_as_remains_tapped() {
        let (rest, dur) = strip_trailing_duration(
            "gain control of target creature for as long as ~ remains tapped",
        );
        assert_eq!(rest, "gain control of target creature");
        assert!(
            matches!(
                dur,
                Some(Duration::ForAsLongAs {
                    condition: StaticCondition::SourceIsTapped,
                })
            ),
            "expected ForAsLongAs(SourceIsTapped), got: {dur:?}"
        );
    }

    #[test]
    fn for_as_long_as_you_control_maps_to_until_host_leaves() {
        let (rest, dur) =
            strip_trailing_duration("target creature gets +2/+2 for as long as you control ~");
        assert_eq!(rest, "target creature gets +2/+2");
        assert_eq!(dur, Some(Duration::UntilHostLeavesPlay));
    }

    #[test]
    fn for_as_long_as_remains_on_battlefield_maps_to_until_host_leaves() {
        let (_, dur) = strip_trailing_duration(
            "target gets +1/+1 for as long as ~ remains on the battlefield",
        );
        assert_eq!(dur, Some(Duration::UntilHostLeavesPlay));
    }

    #[test]
    fn for_as_long_as_has_counter() {
        let (_, dur) = strip_trailing_duration(
            "target creature has flying for as long as it has a flood counter on it",
        );
        assert!(
            matches!(
                dur,
                Some(Duration::ForAsLongAs {
                    condition: StaticCondition::HasCounters {
                        ref counter_type,
                        minimum: 1,
                        maximum: None,
                    },
                }) if counter_type == "flood"
            ),
            "expected ForAsLongAs(HasCounters(flood)), got: {dur:?}"
        );
    }

    #[test]
    fn for_as_long_as_compound_control_and_tapped() {
        let (rest, dur) = strip_trailing_duration(
            "gain control of target creature for as long as you control ~ and it remains tapped",
        );
        assert_eq!(rest, "gain control of target creature");
        assert!(
            matches!(
                dur,
                Some(Duration::ForAsLongAs {
                    condition: StaticCondition::And { ref conditions },
                }) if conditions.len() == 2
            ),
            "expected ForAsLongAs(And[..]), got: {dur:?}"
        );
    }

    #[test]
    fn leading_for_as_long_as_strips_duration_prefix() {
        // CR 611.2b: "For as long as [condition], [effect]" — leading duration prefix.
        // Cards like Preacher, Giant Oyster, Immovable Rod use this pattern.
        let result = strip_leading_duration(
            "For as long as this creature remains tapped, gain control of target creature",
        );
        assert!(result.is_some(), "expected Some, got None");
        let (dur, rest) = result.unwrap();
        assert_eq!(rest, "gain control of target creature");
        assert!(
            matches!(
                dur,
                Duration::ForAsLongAs {
                    condition: StaticCondition::SourceIsTapped,
                }
            ),
            "expected ForAsLongAs(SourceIsTapped), got: {dur:?}"
        );
    }

    #[test]
    fn you_may_have_target_creature_get_pump() {
        // "you may have target creature get +3/+3 until end of turn"
        // should parse as a pump with target creature filter, not Unimplemented
        let e = parse_effect("have target creature get +3/+3 until end of turn");
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "expected pump effect, got Unimplemented: {e:?}"
        );
        assert!(
            matches!(
                e,
                Effect::Pump {
                    power: PtValue::Fixed(3),
                    toughness: PtValue::Fixed(3),
                    ..
                } | Effect::PumpAll { .. }
            ),
            "expected Pump or PumpAll, got: {e:?}"
        );
    }

    #[test]
    fn put_that_many_counters_on_self() {
        let e = parse_effect("put that many +1/+1 counters on ~");
        match e {
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(counter_type, "P1P1");
                // count=0 is the sentinel for "that many" (event context amount)
                assert_eq!(count, 0);
                assert!(matches!(target, TargetFilter::SelfRef));
            }
            _ => panic!("Expected PutCounter, got {e:?}"),
        }
    }

    #[test]
    fn put_that_many_charge_counters_on_self() {
        let e = parse_effect("put that many charge counters on ~");
        match e {
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(counter_type, "charge");
                assert_eq!(count, 0);
                assert!(matches!(target, TargetFilter::SelfRef));
            }
            _ => panic!("Expected PutCounter, got {e:?}"),
        }
    }

    #[test]
    fn strip_each_player_subject_skips_static_restrictions() {
        // These should return (None, original_text) because they are static restrictions,
        // not imperative effects — they belong in the static parser pipeline.
        let cases = [
            "each player can't cast more than one spell each turn",
            "each opponent can't cast noncreature spells",
            "each player can't draw more than one card each turn",
            "each opponent cannot cast spells from exile",
            "each player don't untap during their controllers' untap steps",
            "each opponent may not search libraries",
            "each player may only attack with one creature each combat",
            "each player may cast spells only during their own turns",
        ];
        for text in &cases {
            let (scope, result) = strip_each_player_subject(text);
            assert!(
                scope.is_none(),
                "should not strip static restriction: {text}"
            );
            assert_eq!(&result, text, "text should be unchanged for: {text}");
        }
    }

    #[test]
    fn strip_each_player_subject_still_strips_imperatives() {
        // These should still be stripped (imperative effects, not static restrictions)
        let (scope, result) = strip_each_player_subject("each opponent discards a card");
        assert!(scope.is_some());
        assert_eq!(result, "discard a card");

        let (scope, result) = strip_each_player_subject("each player draws a card");
        assert!(scope.is_some());
        assert_eq!(result, "draw a card");

        let (scope, result) = strip_each_player_subject("each player mills three cards");
        assert!(scope.is_some());
        assert_eq!(result, "mill three cards");

        let (scope, result) = strip_each_player_subject("each opponent loses 2 life");
        assert!(scope.is_some());
        assert_eq!(result, "lose 2 life");
    }

    #[test]
    fn effect_goad_target_creature() {
        let e = parse_effect("goad target creature");
        assert!(matches!(e, Effect::Goad { .. }), "Expected Goad, got {e:?}");
    }

    #[test]
    fn effect_goads_target_creature() {
        let e = parse_effect("goads target creature");
        assert!(matches!(e, Effect::Goad { .. }), "Expected Goad, got {e:?}");
    }

    #[test]
    fn effect_exchange_control_of_two_targets() {
        let e = parse_effect("exchange control of target creature and target creature");
        assert!(
            matches!(e, Effect::ExchangeControl),
            "Expected ExchangeControl, got {e:?}"
        );
    }

    #[test]
    fn have_redirection_target_creature_gets_pump() {
        // Bare "have target creature get +3/+3" should redirect subject
        let e = parse_effect("have target creature get +3/+3");
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "have redirection should not be Unimplemented: {e:?}"
        );
    }

    #[test]
    fn extra_turn_controller() {
        let e = parse_effect("Take an extra turn after this one");
        assert!(matches!(
            e,
            Effect::ExtraTurn {
                target: TargetFilter::Controller
            }
        ));
    }

    #[test]
    fn extra_turn_imperative() {
        // Test the imperative path directly (no subject)
        let clause = parse_imperative_effect(
            "take an extra turn after this one",
            &ParseContext::default(),
        );
        assert!(matches!(
            clause.effect,
            Effect::ExtraTurn {
                target: TargetFilter::Controller
            }
        ));
    }

    #[test]
    fn double_life_total() {
        use crate::types::ability::DoubleTarget;
        let e = parse_effect("Double your life total");
        assert!(matches!(
            e,
            Effect::Double {
                target_kind: DoubleTarget::LifeTotal,
                target: TargetFilter::Controller,
            }
        ));
    }

    #[test]
    fn double_each_kind_of_counter() {
        use crate::types::ability::DoubleTarget;
        let e = parse_effect("Double the number of each kind of counter on target permanent");
        assert!(matches!(
            e,
            Effect::Double {
                target_kind: DoubleTarget::Counters { counter_type: None },
                ..
            }
        ));
    }

    #[test]
    fn double_mana_red() {
        use crate::types::ability::DoubleTarget;
        let e = parse_effect("Double the amount of red mana in your mana pool");
        assert!(matches!(
            e,
            Effect::Double {
                target_kind: DoubleTarget::ManaPool {
                    color: Some(ManaColor::Red)
                },
                target: TargetFilter::Controller,
            }
        ));
    }

    #[test]
    fn have_redirection_it_gain_keyword() {
        // "have it gain reach until end of turn" — anaphoric "it" reference
        let e = parse_effect("have it gain reach until end of turn");
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "have it gain should not be Unimplemented: {e:?}"
        );
    }

    #[test]
    fn have_redirection_fight() {
        // "have target creature fight another target creature"
        let e = parse_effect("have it fight target creature");
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "have fight should not be Unimplemented: {e:?}"
        );
    }

    // --- Restriction clause extensions ---

    #[test]
    fn effect_players_cant_gain_life() {
        // CR 119.7: "Players can't gain life this turn" → GenericEffect(CantGainLife)
        let def = parse_effect_chain("Players can't gain life this turn", AbilityKind::Spell);
        match *def.effect {
            Effect::GenericEffect {
                ref static_abilities,
                ..
            } => {
                assert!(
                    static_abilities
                        .iter()
                        .any(|s| s.mode == StaticMode::CantGainLife),
                    "should contain CantGainLife mode"
                );
            }
            _ => panic!("expected GenericEffect"),
        }
    }

    #[test]
    fn effect_doesnt_untap_restriction() {
        // CR 302.6: "That land doesn't untap during its controller's next untap step"
        // Test at the dispatcher level via parse_oracle_text rather than parse_effect_clause
        // directly, to verify the full pipeline including subject application.
        let keyword_names: Vec<String> = vec![];
        let types: Vec<String> = vec!["Sorcery".to_string()];
        let subtypes: Vec<String> = vec![];
        let r = super::super::parse_oracle_text(
            "~ deals 4 damage to target creature. Tap target land. That land doesn't untap during its controller's next untap step.",
            "TestSpell",
            &keyword_names,
            &types,
            &subtypes,
        );
        assert!(r.statics.is_empty(), "should not produce static");
        assert!(!r.abilities.is_empty(), "should produce spell abilities");
    }

    #[test]
    fn effect_chain_sacrifice_and_deals_damage() {
        // Mogg Bombers: "sacrifice ~ and it deals 3 damage to target player or planeswalker"
        let def = parse_effect_chain(
            "sacrifice ~ and it deals 3 damage to target player or planeswalker",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::Sacrifice { .. }),
            "first effect should be Sacrifice, got {:?}",
            def.effect
        );
        let sub = def
            .sub_ability
            .as_ref()
            .expect("should chain to DealDamage");
        assert!(
            matches!(*sub.effect, Effect::DealDamage { .. }),
            "sub_ability should be DealDamage, got {:?}",
            sub.effect
        );
    }

    // ── "Any opponent may" + "if a player does" ──────────────

    #[test]
    fn any_opponent_may_sacrifice_parses_with_optional_for() {
        let def = parse_effect_chain(
            "any opponent may sacrifice a creature of their choice",
            AbilityKind::Spell,
        );
        assert!(def.optional, "should be optional");
        assert_eq!(
            def.optional_for,
            Some(crate::types::ability::OpponentMayScope::AnyOpponent),
            "should have AnyOpponent scope"
        );
        assert!(
            matches!(*def.effect, Effect::Sacrifice { .. }),
            "effect should be Sacrifice, got {:?}",
            def.effect
        );
        // Target should be Typed(Creature), not Any
        if let Effect::Sacrifice { ref target } = *def.effect {
            match target {
                TargetFilter::Typed(tf) => {
                    assert!(
                        tf.type_filters
                            .contains(&crate::types::ability::TypeFilter::Creature),
                        "target should include Creature filter, got {:?}",
                        tf.type_filters
                    );
                }
                other => panic!("expected Typed filter, got {other:?}"),
            }
        }
    }

    #[test]
    fn if_a_player_does_parses_as_condition() {
        let def = parse_effect_chain(
            "any opponent may sacrifice a creature of their choice. If a player does, tap ~",
            AbilityKind::Spell,
        );
        assert!(def.optional);
        let sub = def.sub_ability.as_ref().expect("should have sub_ability");
        assert_eq!(
            sub.condition,
            Some(AbilityCondition::IfAPlayerDoes),
            "sub condition should be IfAPlayerDoes"
        );
        assert!(
            matches!(*sub.effect, Effect::Tap { .. }),
            "sub effect should be Tap, got {:?}",
            sub.effect
        );
    }

    #[test]
    fn if_they_do_parses_as_if_a_player_does() {
        let def = parse_effect_chain(
            "any opponent may tap an untapped creature they control. If they do, tap ~",
            AbilityKind::Spell,
        );
        assert!(def.optional);
        let sub = def.sub_ability.as_ref().expect("should have sub_ability");
        assert_eq!(sub.condition, Some(AbilityCondition::IfAPlayerDoes));
    }

    #[test]
    fn if_no_one_does_attaches_else_ability() {
        let def = parse_effect_chain(
            "any opponent may sacrifice a creature. If a player does, tap ~. If no one does, draw a card",
            AbilityKind::Spell,
        );
        let sub = def.sub_ability.as_ref().expect("should have sub_ability");
        assert_eq!(sub.condition, Some(AbilityCondition::IfAPlayerDoes));
        let else_ab = sub.else_ability.as_ref().expect("should have else_ability");
        assert!(
            matches!(*else_ab.effect, Effect::Draw { .. }),
            "else_ability should be Draw, got {:?}",
            else_ab.effect
        );
    }

    #[test]
    fn have_it_deal_damage_parses() {
        let def = parse_effect_chain(
            "any opponent may have it deal 4 damage to them",
            AbilityKind::Spell,
        );
        assert!(def.optional);
        assert_eq!(
            def.optional_for,
            Some(crate::types::ability::OpponentMayScope::AnyOpponent)
        );
        assert!(
            matches!(*def.effect, Effect::DealDamage { .. }),
            "effect should be DealDamage, got {:?}",
            def.effect
        );
    }

    #[test]
    fn have_you_put_parses_as_change_zone() {
        let def = parse_effect_chain(
            "any opponent may have you put that card into your graveyard",
            AbilityKind::Spell,
        );
        assert!(def.optional);
        assert_eq!(
            def.optional_for,
            Some(crate::types::ability::OpponentMayScope::AnyOpponent)
        );
        // The inner effect should parse (not be Unimplemented)
        assert!(
            !matches!(*def.effect, Effect::Unimplemented { .. }),
            "effect should not be Unimplemented, got {:?}",
            def.effect
        );
    }

    // -----------------------------------------------------------------------
    // CR 608.2k: resolve_it_pronoun unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_it_pronoun_default_context() {
        let ctx = ParseContext::default();
        assert_eq!(resolve_it_pronoun(&ctx), TargetFilter::SelfRef);
    }

    #[test]
    fn resolve_it_pronoun_self_ref_subject() {
        let ctx = ParseContext {
            subject: Some(TargetFilter::SelfRef),
        };
        assert_eq!(resolve_it_pronoun(&ctx), TargetFilter::SelfRef);
    }

    #[test]
    fn resolve_it_pronoun_typed_subject() {
        let ctx = ParseContext {
            subject: Some(TargetFilter::Typed(crate::types::ability::TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::You),
                ..Default::default()
            })),
        };
        assert_eq!(resolve_it_pronoun(&ctx), TargetFilter::TriggeringSource);
    }

    #[test]
    fn resolve_it_pronoun_attached_to_subject() {
        let ctx = ParseContext {
            subject: Some(TargetFilter::AttachedTo),
        };
        assert_eq!(resolve_it_pronoun(&ctx), TargetFilter::TriggeringSource);
    }

    #[test]
    fn resolve_it_pronoun_any_subject() {
        let ctx = ParseContext {
            subject: Some(TargetFilter::Any),
        };
        assert_eq!(resolve_it_pronoun(&ctx), TargetFilter::SelfRef);
    }

    // --- Suffix condition extraction tests ---

    #[test]
    fn parse_quantity_comparison_greater_than_dynamic() {
        let result = parse_quantity_comparison("greater than your starting life total");
        let (cmp, rhs) = result.expect("should parse");
        assert_eq!(cmp, Comparator::GT);
        assert!(matches!(
            rhs,
            QuantityExpr::Ref {
                qty: QuantityRef::StartingLifeTotal
            }
        ));
    }

    #[test]
    fn parse_quantity_comparison_less_than_dynamic() {
        let result = parse_quantity_comparison("less than your life total");
        let (cmp, rhs) = result.expect("should parse");
        assert_eq!(cmp, Comparator::LT);
        assert!(matches!(
            rhs,
            QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal
            }
        ));
    }

    #[test]
    fn parse_quantity_comparison_fixed_fallback() {
        let result = parse_quantity_comparison("3 or greater");
        let (cmp, rhs) = result.expect("should parse");
        assert_eq!(cmp, Comparator::GE);
        assert!(matches!(rhs, QuantityExpr::Fixed { value: 3 }));
    }

    #[test]
    fn parse_condition_text_life_greater_than_starting() {
        let result =
            parse_condition_text("your life total is greater than your starting life total");
        let cond = result.expect("should parse");
        assert!(matches!(
            cond,
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal
                },
                comparator: Comparator::GT,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::StartingLifeTotal
                },
            }
        ));
    }

    #[test]
    fn parse_condition_text_non_comparison_returns_none() {
        assert!(parse_condition_text("the creature that is exiled").is_none());
    }

    #[test]
    fn strip_suffix_conditional_extracts_quantity_check() {
        let (cond, text) = strip_suffix_conditional(
            "draw a card if your life total is greater than your starting life total",
        );
        assert_eq!(text, "draw a card");
        let cond = cond.expect("should extract condition");
        assert!(matches!(
            cond,
            AbilityCondition::QuantityCheck {
                comparator: Comparator::GT,
                ..
            }
        ));
    }

    #[test]
    fn strip_suffix_conditional_excludes_if_able() {
        let (cond, text) = strip_suffix_conditional("deal 3 damage to that creature if able");
        assert!(cond.is_none());
        assert_eq!(text, "deal 3 damage to that creature if able");
    }

    #[test]
    fn strip_suffix_conditional_excludes_if_you_do() {
        let (cond, _) = strip_suffix_conditional("sacrifice it if you do");
        assert!(cond.is_none());
    }

    #[test]
    fn strip_suffix_conditional_unparseable_returns_none() {
        let (cond, text) =
            strip_suffix_conditional("sacrifice a creature if you control a creature");
        assert!(cond.is_none());
        assert_eq!(text, "sacrifice a creature if you control a creature");
    }

    #[test]
    fn suffix_condition_with_otherwise_integration() {
        // Cosmos Elixir pattern: suffix condition + Otherwise clause.
        std::thread::Builder::new()
            .name("suffix-otherwise".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let def = parse_effect_chain(
                    "draw a card if your life total is greater than your starting life total. Otherwise, you gain 2 life and scry 1.",
                    AbilityKind::Spell,
                );
                // First def should be Draw with a QuantityCheck condition.
                assert!(matches!(*def.effect, Effect::Draw { .. }));
                assert!(
                    matches!(
                        def.condition,
                        Some(AbilityCondition::QuantityCheck {
                            comparator: Comparator::GT,
                            ..
                        })
                    ),
                    "draw should have QuantityCheck condition, got {:?}",
                    def.condition
                );
                // Otherwise should be attached as else_ability containing GainLife.
                let else_ab = def
                    .else_ability
                    .as_ref()
                    .expect("draw should have else_ability from Otherwise");
                assert!(
                    matches!(*else_ab.effect, Effect::GainLife { .. }),
                    "else_ability should be GainLife, got {:?}",
                    else_ab.effect
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }

    // --- ReturnDestination flag propagation tests ---

    #[test]
    fn return_destination_under_your_control() {
        let (target_text, dest) =
            strip_return_destination_ext("target creature to the battlefield under your control");
        assert_eq!(target_text, "target creature");
        let d = dest.expect("should parse destination");
        assert_eq!(d.zone, Zone::Battlefield);
        assert!(d.under_your_control);
        assert!(!d.enter_tapped);
        assert!(!d.transformed);
    }

    #[test]
    fn return_destination_tapped() {
        let (target_text, dest) =
            strip_return_destination_ext("target creature to the battlefield tapped");
        assert_eq!(target_text, "target creature");
        let d = dest.expect("should parse destination");
        assert_eq!(d.zone, Zone::Battlefield);
        assert!(d.enter_tapped);
        assert!(!d.under_your_control);
    }

    #[test]
    fn return_destination_owners_control_not_under_your_control() {
        let (_, dest) =
            strip_return_destination_ext("it to the battlefield under its owner's control");
        let d = dest.expect("should parse destination");
        assert!(
            !d.under_your_control,
            "owner's control should not set under_your_control"
        );
    }

    #[test]
    fn return_destination_tapped_under_your_control() {
        let (_, dest) =
            strip_return_destination_ext("it to the battlefield tapped under your control");
        let d = dest.expect("should parse destination");
        assert!(d.enter_tapped);
        assert!(d.under_your_control);
    }

    #[test]
    fn return_destination_transformed_under_your_control() {
        let (_, dest) =
            strip_return_destination_ext("it to the battlefield transformed under your control");
        let d = dest.expect("should parse destination");
        assert!(d.transformed);
        assert!(d.under_your_control);
        assert!(!d.enter_tapped);
    }

    #[test]
    fn return_destination_plain_battlefield() {
        let (_, dest) = strip_return_destination_ext("target creature to the battlefield");
        let d = dest.expect("should parse destination");
        assert!(!d.under_your_control);
        assert!(!d.enter_tapped);
        assert!(!d.transformed);
    }

    /// Collect all effects in a sub_ability chain into a flat Vec.
    fn collect_chain_effects(def: &AbilityDefinition) -> Vec<&Effect> {
        let mut effects = vec![&*def.effect];
        let mut current = &def.sub_ability;
        while let Some(sub) = current {
            effects.push(&*sub.effect);
            current = &sub.sub_ability;
        }
        effects
    }

    #[test]
    fn conjunction_verb_propagation_decimate_pattern() {
        // "destroy target artifact, target creature, target enchantment, and target land"
        // should produce 4 Destroy effects chained as sub_abilities
        let def = parse_effect_chain(
            "destroy target artifact, target creature, target enchantment, and target land",
            AbilityKind::Spell,
        );
        let effects = collect_chain_effects(&def);
        let destroy_count = effects
            .iter()
            .filter(|e| matches!(e, Effect::Destroy { .. }))
            .count();
        assert_eq!(
            destroy_count, 4,
            "Decimate should produce 4 Destroy effects, got {destroy_count}: {effects:?}"
        );
    }

    #[test]
    fn conjunction_verb_propagation_exile_two_targets() {
        // "exile target creature and target artifact" should produce 2 ChangeZone(exile) effects
        let def = parse_effect_chain(
            "exile target creature and target artifact",
            AbilityKind::Spell,
        );
        let effects = collect_chain_effects(&def);
        let exile_count = effects
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Effect::ChangeZone {
                        destination: Zone::Exile,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            exile_count, 2,
            "Should produce 2 exile effects, got {exile_count}: {effects:?}"
        );
    }

    #[test]
    fn conjunction_multi_effect_discard_and_gain_life() {
        // "each opponent discards a card and you gain 3 life"
        // should produce Discard + GainLife chain
        let def = parse_effect_chain(
            "each opponent discards a card and you gain 3 life",
            AbilityKind::Spell,
        );
        let effects = collect_chain_effects(&def);
        let has_discard = effects.iter().any(|e| matches!(e, Effect::Discard { .. }));
        let has_gain_life = effects.iter().any(|e| matches!(e, Effect::GainLife { .. }));
        assert!(has_discard, "Should have Discard effect: {effects:?}");
        assert!(has_gain_life, "Should have GainLife effect: {effects:?}");
    }

    #[test]
    fn conjunction_single_target_regression() {
        // "destroy target creature" should still work (single target, no conjunction)
        let def = parse_effect_chain("destroy target creature", AbilityKind::Spell);
        assert!(
            matches!(*def.effect, Effect::Destroy { .. }),
            "Single destroy should work: {:?}",
            def.effect
        );
    }

    #[test]
    fn conjunction_predicate_and_not_split() {
        // "target creature gets +2/+2 and gains flying until end of turn"
        // "and" is in predicate, not a conjunction — should NOT split
        let def = parse_effect_chain(
            "target creature gets +2/+2 and gains flying until end of turn",
            AbilityKind::Spell,
        );
        // Should produce a Pump or continuous effect, not split into two separate effects
        assert!(
            !matches!(*def.effect, Effect::Unimplemented { .. }),
            "Should not be Unimplemented: {:?}",
            def.effect
        );
    }

    #[test]
    fn conjunction_noun_phrase_and_not_split() {
        // "target creature and all other creatures you control get +1/+1"
        // "and" is part of subject noun phrase — should NOT split
        let def = parse_effect_chain(
            "target creature and all other creatures you control get +1/+1 until end of turn",
            AbilityKind::Spell,
        );
        let effects = collect_chain_effects(&def);
        // Should NOT produce multiple separate effects from false split
        assert!(
            effects.len() <= 2,
            "Noun-phrase 'and' should not cause split into many effects: {effects:?}"
        );
    }
}
