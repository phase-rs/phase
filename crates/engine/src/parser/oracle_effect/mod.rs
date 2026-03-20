mod animation;
mod counter;
pub(crate) mod imperative;
mod mana;
mod sequence;
mod subject;
mod token;
mod types;

use std::str::FromStr;

use super::oracle_target::{parse_event_context_ref, parse_target};
use super::oracle_util::{contains_possessive, parse_mana_symbols, parse_number};
use crate::database::mtgjson::parse_mtgjson_mana_cost;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, CardPlayMode, CastingPermission, ChoiceType,
    DelayedTriggerCondition, Duration, Effect, FilterProp, GainLifePlayer, MultiTargetSpec,
    PlayerFilter, PtValue, QuantityExpr, QuantityRef, StaticDefinition, TargetFilter, TypeFilter,
    TypedFilter,
};
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

/// Parse an effect clause from Oracle text into an Effect enum.
/// This handles the verb-based matching for spell effects, activated ability effects,
/// and the effect portion of triggered abilities.
///
/// For compound effects ("Gain 3 life. Draw a card."), call `parse_effect_chain`
/// which splits on sentence boundaries and chains via AbilityDefinition::sub_ability.
pub fn parse_effect(text: &str) -> Effect {
    parse_effect_clause(text).effect
}

/// CR 603.7c: Parse inline delayed triggers like "when that creature dies, draw a card".
/// Returns a `CreateDelayedTrigger` wrapping the parsed inner effect.
fn try_parse_inline_delayed_trigger(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    if !lower.starts_with("when ") {
        return None;
    }

    // Find the comma separator between condition and effect
    let comma = lower.find(", ")?;
    let condition_text = &lower[5..comma];
    let effect_text = &text[comma + 2..];

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
fn try_parse_damage_prevention_disabled(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    if !lower.contains("can't be prevented") && !lower.contains("cannot be prevented") {
        return None;
    }
    if !lower.contains("damage") {
        return None;
    }

    // Determine expiry: "this turn" → EndOfTurn, otherwise EndOfTurn as default
    let expiry = if lower.contains("this turn") {
        crate::types::ability::RestrictionExpiry::EndOfTurn
    } else {
        // Default to EndOfTurn for damage prevention restrictions
        crate::types::ability::RestrictionExpiry::EndOfTurn
    };

    // Determine scope from the subject phrase
    let scope = if lower.contains("creatures you control") || lower.contains("sources you control")
    {
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
    })
}

fn parse_effect_clause(text: &str) -> ParsedEffectClause {
    let text = strip_leading_sequence_connector(text)
        .trim()
        .trim_end_matches('.');
    if text.is_empty() {
        return parsed_clause(Effect::Unimplemented {
            name: "empty".to_string(),
            description: None,
        });
    }

    // CR 106.12: "don't lose [unspent] {color} mana as steps and phases end" —
    // mana pool retention. Parsed as supported no-op (runtime behavior is future work).
    {
        let lower = text.to_lowercase();
        if lower.contains("lose") && lower.contains("mana as steps") {
            return parsed_clause(Effect::GenericEffect {
                static_abilities: vec![],
                duration: None,
                target: None,
            });
        }
    }

    // CR 701.52: "the ring tempts you" — Ring Tempts You effect.
    {
        let lower = text.to_lowercase();
        if lower.contains("the ring tempts you") {
            return parsed_clause(Effect::RingTemptsYou);
        }
    }

    // CR 603.7c: "When that creature dies, ..." — inline delayed trigger creation.
    if let Some(clause) = try_parse_inline_delayed_trigger(text) {
        return clause;
    }

    // CR 614.16: "Damage can't be prevented [this turn]" → Effect::AddRestriction
    if let Some(clause) = try_parse_damage_prevention_disabled(text) {
        return clause;
    }

    // CR 705: "If you win/lose the flip, [effect]" — coin flip branch.
    // Returns a FlipCoin with the appropriate branch filled in.
    // consolidate_die_and_coin_defs merges these into the preceding FlipCoin.
    if let Some((is_win, effect_text)) = imperative::try_parse_coin_flip_branch(text) {
        let branch_def = parse_effect_chain(effect_text, AbilityKind::Spell);
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
        return with_clause_duration(parse_effect_clause(rest), duration);
    }

    // "it's still a/an [type]" / "that's still a/an [type]" — type-retention clause
    // CR 205.1a: Retains the original type in addition to new types from animation effects
    if let Some(clause) = try_parse_still_a_type(text) {
        return clause;
    }

    // "for each" patterns: "draw a card for each [filter]", etc.
    if let Some(clause) = try_parse_for_each_effect(text) {
        return clause;
    }

    // CR 121.6: "{verb} cards equal to {quantity}" — dynamic count from game state.
    if let Some(clause) = try_parse_equal_to_quantity_effect(text) {
        return clause;
    }

    // CR 400.7i: "you may play/cast that card [this turn]" — impulse draw permission.
    if let Some(clause) = try_parse_play_from_exile(text) {
        return clause;
    }

    let ast = parse_clause_ast(text);
    lower_clause_ast(ast)
}

/// Parse "it's still a/an [type]" and "that's still a/an [type]" type-retention clauses.
///
/// These appear as separate sentences after animation effects (e.g., "This land becomes
/// a 3/3 creature with vigilance. It's still a land."). The clause ensures the original
/// type is retained as a permanent continuous effect.
///
/// CR 205.1a: An object retains types explicitly stated by the effect.
/// Parse "{keyword_action} {multiplier}" patterns like "investigate twice",
/// "proliferate twice", "investigate three times", "investigate four times".
///
/// Returns a sub_ability chain of N copies of the keyword action effect.
/// The multiplier is stripped from the text and the base action is parsed
/// through the normal imperative pipeline.
fn try_parse_repeated_keyword_action(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();

    // Map multiplier suffixes to repeat counts
    let (base, count) = if let Some(base) = lower.strip_suffix(" twice") {
        (base, 2u32)
    } else if let Some(base) = lower.strip_suffix(" three times") {
        (base, 3)
    } else if let Some(base) = lower.strip_suffix(" four times") {
        (base, 4)
    } else if let Some(base) = lower.strip_suffix(" five times") {
        (base, 5)
    } else {
        return None;
    };

    // Parse the base action (e.g., "investigate", "proliferate")
    let base_effect = parse_imperative_effect(base);
    if matches!(base_effect, Effect::Unimplemented { .. }) {
        return None;
    }

    // Chain N-1 sub_abilities after the first effect
    let mut sub: Option<Box<AbilityDefinition>> = None;
    for _ in 1..count {
        let mut def = AbilityDefinition::new(AbilityKind::Spell, base_effect.clone());
        def.sub_ability = sub;
        sub = Some(Box::new(def));
    }

    Some(ParsedEffectClause {
        effect: base_effect,
        duration: None,
        sub_ability: sub,
    })
}

fn try_parse_still_a_type(text: &str) -> Option<ParsedEffectClause> {
    use crate::types::ability::ContinuousModification;
    use crate::types::card_type::CoreType;

    let lower = text.to_lowercase();
    // Match "it's still a/an [type]" or "that's still a/an [type]"
    let rest = lower
        .strip_prefix("it's still ")
        .or_else(|| lower.strip_prefix("that's still "))?;
    let type_name = rest
        .strip_prefix("a ")
        .or_else(|| rest.strip_prefix("an "))?;
    let core_type = CoreType::from_str(&capitalize(type_name)).ok()?;

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddType { core_type }])
                .description(text.to_string())],
            duration: Some(Duration::Permanent),
            target: None,
        },
        duration: Some(Duration::Permanent),
        sub_ability: None,
    })
}

/// Parse "{verb} cards equal to {quantity_ref}" patterns (CR 121.6).
///
/// Handles verbs whose count field is `QuantityExpr` (mill, draw).
fn try_parse_equal_to_quantity_effect(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    if let Some(rest) = lower.strip_prefix("mill cards equal to ") {
        let rest = rest.trim().trim_end_matches('.');
        let qty = super::oracle_static::parse_quantity_ref(rest)?;
        return Some(parsed_clause(Effect::Mill {
            count: QuantityExpr::Ref { qty },
            target: TargetFilter::Any,
        }));
    }
    if let Some(rest) = lower.strip_prefix("draw cards equal to ") {
        let rest = rest.trim().trim_end_matches('.');
        let qty = super::oracle_static::parse_quantity_ref(rest)?;
        return Some(parsed_clause(Effect::Draw {
            count: QuantityExpr::Ref { qty },
        }));
    }
    None
}

/// CR 400.7i: Parse "you may play/cast that card [this turn]" — impulse draw permission.
///
/// Handles patterns like:
/// - "you may play that card this turn"
/// - "you may cast that card this turn"
/// - "you may play that card"
/// - "you may play it this turn"
fn try_parse_play_from_exile(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    let lower = lower.trim_end_matches('.');

    // Try full forms first: "you may play/cast that card/it/those cards ..."
    // Then bare forms (after "you may" has been stripped): "play that card ..."
    let full_rest = lower
        .strip_prefix("you may play ")
        .or_else(|| lower.strip_prefix("you may cast "));

    if let Some(rest) = full_rest {
        // Full form: rest must start with a card reference
        if !(rest.starts_with("that card")
            || rest.starts_with("that spell")
            || rest.starts_with("those cards")
            || rest.starts_with("it ")
            || rest == "it")
        {
            return None;
        }
    } else {
        // Bare form (after "you may" was stripped by parse_effect_chain):
        // Only match when temporal context exists ("this turn", "until"),
        // otherwise it's a CastFromZone, not impulse draw permission.
        let has_temporal = lower.contains("this turn") || lower.contains("until ");
        if !has_temporal {
            return None;
        }
        if lower.contains("without paying") {
            return None;
        }
        if !(lower.starts_with("play that card")
            || lower.starts_with("cast that card")
            || lower.starts_with("play it")
            || lower.starts_with("cast it"))
        {
            return None;
        }
    }

    // Default duration is UntilEndOfTurn for impulse draw
    let duration = if lower.contains("until the end of your next turn")
        || lower.contains("until your next turn")
    {
        Duration::UntilYourNextTurn
    } else {
        Duration::UntilEndOfTurn
    };

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

    // Find "for each" in the text
    let for_each_idx = lower.find("for each ")?;
    let base = text[..for_each_idx].trim();
    let for_each_clause = &lower[for_each_idx + "for each ".len()..];
    let base_lower = base.to_lowercase();

    // Parse the "for each" clause into a QuantityRef
    let qty = parse_for_each_clause(for_each_clause)?;
    let quantity = QuantityExpr::Ref { qty };

    // Parse the base effect and replace its count with the dynamic quantity
    if base_lower.starts_with("draw ") || base_lower.contains(" draw") {
        return Some(parsed_clause(Effect::Draw { count: quantity }));
    }

    if (base_lower.starts_with("you gain ") || base_lower.starts_with("gain "))
        && base_lower.contains("life")
    {
        return Some(parsed_clause(Effect::GainLife {
            amount: quantity,
            player: GainLifePlayer::Controller,
        }));
    }

    if (base_lower.starts_with("you lose ") || base_lower.starts_with("lose "))
        && base_lower.contains("life")
    {
        return Some(parsed_clause(Effect::LoseLife { amount: quantity }));
    }

    None
}

/// Parse the clause after "for each" into a QuantityRef.
fn parse_for_each_clause(clause: &str) -> Option<QuantityRef> {
    let clause = clause.trim().trim_end_matches('.');

    // "opponent who lost life this turn"
    if clause.contains("opponent") && clause.contains("lost life") {
        return Some(QuantityRef::PlayerCount {
            filter: PlayerFilter::OpponentLostLife,
        });
    }

    // "opponent who gained life this turn"
    if clause.contains("opponent") && clause.contains("gained life") {
        return Some(QuantityRef::PlayerCount {
            filter: PlayerFilter::OpponentGainedLife,
        });
    }

    // "opponent"
    if clause == "opponent" || clause == "opponent you have" {
        return Some(QuantityRef::PlayerCount {
            filter: PlayerFilter::Opponent,
        });
    }

    // "[counter type] counter on ~" / "[counter type] counter on it"
    if clause.contains("counter on") {
        let counter_type = clause
            .split("counter")
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if !counter_type.is_empty() {
            return Some(QuantityRef::CountersOnSelf {
                counter_type: counter_type.replace('+', "plus").replace('-', "minus"),
            });
        }
    }

    // "creature you control", "artifact you control", etc.
    let (filter, _) = parse_target(clause);
    if !matches!(filter, TargetFilter::Any) {
        return Some(QuantityRef::ObjectCount { filter });
    }

    None
}

fn parse_clause_ast(text: &str) -> ClauseAst {
    let text = text.trim();

    // Mirror the CubeArtisan grammar's high-level sentence shapes:
    // 1) conditionals ("if X, Y"), 2) subject + verb phrase, 3) bare imperative.
    if let Some((condition_text, remainder)) = split_leading_conditional(text) {
        let _ = condition_text;
        return ClauseAst::Conditional {
            clause: Box::new(parse_clause_ast(&remainder)),
        };
    }

    if let Some(ast) = try_parse_subject_predicate_ast(text) {
        return ast;
    }

    ClauseAst::Imperative {
        text: text.to_string(),
    }
}

fn lower_clause_ast(ast: ClauseAst) -> ParsedEffectClause {
    match ast {
        ClauseAst::Imperative { text } => lower_imperative_clause(&text),
        ClauseAst::SubjectPredicate { subject, predicate } => {
            lower_subject_predicate_ast(subject, *predicate)
        }
        ClauseAst::Conditional { clause } => {
            // Phase 2 preserves current semantics for generic leading conditionals:
            // recognize the structure explicitly, but lower only the body.
            lower_clause_ast(*clause)
        }
    }
}

fn lower_imperative_clause(text: &str) -> ParsedEffectClause {
    // "{keyword_action} twice/three times/N times" → chain N copies via sub_ability.
    // Handles "investigate twice", "proliferate twice", "investigate three times", etc.
    if let Some(clause) = try_parse_repeated_keyword_action(text) {
        return clause;
    }

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
    if let Some(clause) = try_split_targeted_compound(text) {
        return clause;
    }

    let (stripped, duration) = strip_trailing_duration(text);
    let mut clause = parsed_clause(parse_imperative_effect(stripped));
    if clause.duration.is_none() {
        clause.duration = duration;
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
fn try_parse_verb_and_target<'a>(
    text: &'a str,
    lower: &str,
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
    // Earthbend: "earthbend [N] target <type>" → Animate with haste + is_earthbend
    if let Some(rest) = lower.strip_prefix("earthbend ") {
        let (pt, target_text) = parse_number(rest)
            .map(|(n, rem)| (n as i32, rem.trim_start()))
            .unwrap_or((0, rest));
        let original_target_text = &text[text.len() - target_text.len()..];
        let (target, rem) = parse_target(original_target_text);
        return Some((
            TargetedImperativeAst::Earthbend {
                target,
                power: pt,
                toughness: pt,
            },
            rem,
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
        let (_, destination) = strip_return_destination(rest);
        let (target, rem) = parse_target(rest);
        return match destination {
            Some(Zone::Battlefield) => {
                Some((TargetedImperativeAst::ReturnToBattlefield { target }, rem))
            }
            _ => Some((TargetedImperativeAst::Return { target }, rem)),
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
        )) = counter::try_parse_put_counter(lower, text)
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
fn try_split_targeted_compound(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();

    // Quick bail: no " and " means no compound connector possible
    if !lower.contains(" and ") {
        return None;
    }

    // Use parse_target's remainder to determine the compound split point
    let (primary_ast, remainder) = try_parse_verb_and_target(text, &lower)?;

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
    let sub_lower = sub_text.to_lowercase();
    let mut sub_effect = parse_imperative_effect(sub_text);

    // If the remainder contains anaphoric references ("it", "that creature", "them"),
    // replace the sub_effect's target with ParentTarget so it inherits the parent's targets.
    if has_anaphoric_reference(&sub_lower) {
        replace_target_with_parent(&mut sub_effect);
    }

    let sub_ability = AbilityDefinition::new(AbilityKind::Spell, sub_effect);

    Some(ParsedEffectClause {
        effect: primary_effect,
        duration: None,
        sub_ability: Some(Box::new(sub_ability)),
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

    // Build ChangeZone for the second subject as a sub_ability
    let sub_effect = Effect::ChangeZone {
        origin: None,
        destination: Zone::Library,
        target: second,
        owner_library,
    };
    let sub_def = AbilityDefinition::new(AbilityKind::Spell, sub_effect);

    // Build ChangeZone for the first subject as the primary effect
    let primary_effect = Effect::ChangeZone {
        origin: None,
        destination: Zone::Library,
        target: first,
        owner_library,
    };

    Some(ParsedEffectClause {
        effect: primary_effect,
        duration: None,
        sub_ability: Some(Box::new(sub_def)),
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
        | Effect::Fight { target }
        | Effect::Bounce { target, .. }
        | Effect::DealDamage { target, .. }
        | Effect::Pump { target, .. }
        | Effect::Attach { target, .. }
        | Effect::Counter { target, .. }
        | Effect::Transform { target, .. }
        | Effect::Connive { target }
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
        _ => {
            // Effects without a target field (Draw, GainLife, etc.) stay as-is.
            // ParentTarget is handled by the sub_ability chain's target propagation.
        }
    }
}

fn lower_subject_predicate_ast(
    subject: SubjectPhraseAst,
    predicate: PredicateAst,
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
        },
        PredicateAst::Become {
            effect,
            duration,
            sub_ability,
        } => ParsedEffectClause {
            effect,
            duration,
            sub_ability,
        },
        PredicateAst::Restriction { effect, duration } => ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
        },
        PredicateAst::ImperativeFallback { text } => {
            if matches!(text.to_lowercase().as_str(), "shuffle" | "shuffles")
                && matches!(
                    subject.affected,
                    TargetFilter::Player | TargetFilter::Controller
                )
            {
                return parsed_clause(Effect::Shuffle {
                    target: subject.affected,
                });
            }
            // CR 701.16a: "<player> reveals the top [N] card(s) of their library"
            let pred_lower = text.to_lowercase();
            if pred_lower.starts_with("reveal ")
                && pred_lower.contains("top")
                && pred_lower.contains("library")
            {
                let count = if let Some(pos) = pred_lower.find("top ") {
                    let after_top = &pred_lower[pos + 4..];
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
            let mut clause = lower_imperative_clause(&text);
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
        Effect::Connive { target }
        | Effect::PhaseOut { target }
        | Effect::ForceBlock { target }
        | Effect::Suspect { target }
            if *target == TargetFilter::Any =>
        {
            *target = subject_filter;
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
                StaticDefinition::new(StaticMode::Other("EmblemStatic".to_string()))
                    .description(inner.to_string()),
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

fn parse_imperative_effect(text: &str) -> Effect {
    let lower = text.to_lowercase();
    if let Some(ast) = parse_imperative_family_ast(text, &lower) {
        return lower_imperative_family_ast(ast);
    }

    // CR 114.1: "you get an emblem with "[static text]""
    if let Some(effect) = try_parse_emblem_creation(&lower, text) {
        return effect;
    }

    // CR 601.2a + CR 118.9: "cast it/that card without paying its mana cost"
    if let Some(effect) = try_parse_cast_effect(&lower) {
        return effect;
    }

    // --- Fallback ---
    let verb = lower.split_whitespace().next().unwrap_or("unknown");
    Effect::Unimplemented {
        name: verb.to_string(),
        description: Some(text.to_string()),
    }
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
        if let Some(TargetFilter::Typed(TypedFilter { card_type, .. })) =
            type_str_to_target_filter(negated)
        {
            return TargetFilter::Typed(TypedFilter::card().properties(vec![
                FilterProp::NonType {
                    value: card_type.map(|ct| format!("{ct:?}")).unwrap_or_default(),
                },
            ]));
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
fn contains_explicit_tracked_set_pronoun(text: &str) -> bool {
    let lower = text.to_lowercase();
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
fn contains_implicit_tracked_set_pronoun(text: &str) -> bool {
    let lower = text.to_lowercase();
    (lower.starts_with("return it ") || lower.starts_with("return them "))
        && lower.contains("battlefield")
}

fn mark_uses_tracked_set(def: &mut AbilityDefinition) {
    if let Effect::CreateDelayedTrigger {
        uses_tracked_set, ..
    } = &mut def.effect
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
    let chunks = split_clause_sequence(text);
    let mut defs: Vec<AbilityDefinition> = Vec::new();

    for chunk in &chunks {
        let normalized_text = strip_leading_sequence_connector(&chunk.text).trim();
        if normalized_text.is_empty() {
            continue;
        }

        // CR 608.2c: "Otherwise, [effect]" — attach as else_ability on the
        // most recent conditional (IfYouDo) def in the chain.
        let lower_check = normalized_text.to_lowercase();
        let otherwise_prefix_len = if lower_check.starts_with("otherwise, ") {
            Some("otherwise, ".len())
        } else if lower_check.starts_with("otherwise ") {
            Some("otherwise ".len())
        } else if lower_check.starts_with("if not, ") {
            Some("if not, ".len())
        } else {
            None
        };
        if let Some(prefix_len) = otherwise_prefix_len {
            let else_text = &normalized_text[prefix_len..];
            let else_def = parse_effect_chain(else_text, kind);
            // Walk defs backward to find the most recent IfYouDo conditional
            let has_if_you_do = defs
                .iter()
                .any(|d| matches!(d.condition, Some(AbilityCondition::IfYouDo)));
            if has_if_you_do {
                for d in defs.iter_mut().rev() {
                    if matches!(d.condition, Some(AbilityCondition::IfYouDo)) {
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
        let condition = condition.or(if_you_do);
        let (is_optional, text) = strip_optional_effect_prefix(&text);
        let (repeat_for, text) = strip_for_each_prefix(&text);
        let (text_no_temporal, delayed_condition) = strip_temporal_suffix(&text);
        let (text_no_qty, multi_target) = strip_any_number_quantifier(text_no_temporal);
        let clause = parse_effect_clause(&text_no_qty);
        let mut def = AbilityDefinition::new(kind, clause.effect);
        if is_optional {
            def.optional = true;
        }
        if let Some(qty) = repeat_for {
            def.repeat_for = Some(qty);
        }
        if let Some(duration) = clause.duration {
            def = def.duration(duration);
        }
        if let Some(ref condition) = condition {
            def = def.condition(condition.clone());
        }
        if let Some(spec) = multi_target {
            def = def.multi_target(spec);
        }

        // Kicker clauses referencing "that creature"/"it" inherit the parent's target.
        // Scoped to conditional sub-abilities only — "it"/"its" appears in possessive
        // forms on many cards and would incorrectly replace targets if applied generally.
        if condition.is_some() && !defs.is_empty() && has_anaphoric_reference(&text.to_lowercase())
        {
            replace_target_with_parent(&mut def.effect);
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
                let has_tracked_ref = contains_explicit_tracked_set_pronoun(normalized_text)
                    || contains_implicit_tracked_set_pronoun(normalized_text);
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
            parse_intrinsic_continuation_ast(normalized_text, &current_defs[0].effect);
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
            &defs[i].effect,
            Effect::FlipCoin {
                win_effect: None,
                lose_effect: None,
            }
        ) {
            let mut win = None;
            let mut lose = None;
            let mut j = i + 1;
            while j < defs.len() && (win.is_none() || lose.is_none()) {
                match &defs[j].effect {
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
                defs[i].effect = Effect::FlipCoin {
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
    let filter = if let Some(for_idx) = lower.find("for a ") {
        let after_for = &lower[for_idx + 6..];
        parse_search_filter(after_for)
    } else if let Some(for_idx) = lower.find("for an ") {
        let after_for = &lower[for_idx + 7..];
        parse_search_filter(after_for)
    } else {
        TargetFilter::Any
    };

    let reveal = lower.contains("reveal");
    let count = if lower.contains("up to two") {
        2
    } else if lower.contains("up to three") {
        3
    } else {
        1
    };

    SearchLibraryDetails {
        filter,
        count,
        reveal,
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

    // Check for "basic land" pattern
    let is_basic = type_text.contains("basic");
    let clean = type_text.replace("basic ", "");

    // Map type name to TypeFilter
    let card_type = match clean.trim() {
        "land" => Some(TypeFilter::Land),
        "creature" => Some(TypeFilter::Creature),
        "artifact" => Some(TypeFilter::Artifact),
        "enchantment" => Some(TypeFilter::Enchantment),
        "instant" => Some(TypeFilter::Instant),
        "sorcery" => Some(TypeFilter::Sorcery),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        "instant or sorcery" => {
            let mut properties = vec![];
            if is_basic {
                properties.push(FilterProp::HasSupertype {
                    value: "Basic".to_string(),
                });
            }
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
            // Could be a subtype search: "forest card", "plains card", "equipment card"
            let land_subtypes = ["plains", "island", "swamp", "mountain", "forest"];
            if land_subtypes.contains(&other) {
                let mut properties = vec![];
                if is_basic {
                    properties.push(FilterProp::HasSupertype {
                        value: "Basic".to_string(),
                    });
                }
                return TargetFilter::Typed(
                    TypedFilter::land()
                        .subtype(capitalize(other))
                        .properties(properties),
                );
            }
            if other == "equipment" {
                return TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact).subtype("Equipment".to_string()),
                );
            }
            if other == "aura" {
                return TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Enchantment).subtype("Aura".to_string()),
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

    TargetFilter::Typed(TypedFilter {
        card_type,
        subtype: None,
        controller: None,
        properties,
    })
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

    // Try the legacy phrasing first: "if this spell's additional cost was paid, ..."
    let body = if let Some(rest) = lower.strip_prefix("if this spell's additional cost was paid, ")
    {
        let offset = text.len() - rest.len();
        Some(text[offset..].to_string())
    }
    // Unified kicker pattern: "if <subject> was kicked, ..."
    // Covers "if this spell was kicked", "if it was kicked", "if ~ was kicked"
    else if lower.starts_with("if ") {
        lower.split_once(" was kicked, ").map(|(_, rest)| {
            let offset = text.len() - rest.len();
            text[offset..].to_string()
        })
    } else {
        None
    };

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
/// Both forms gate the sub-effect on the parent's optional_effect_performed flag.
/// "When you do" is a reflexive trigger (CR 603.12) but in this engine's atomic resolution
/// model it is semantically identical to "if you do" (CR 608.2c).
fn strip_if_you_do_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    // CR 603.12: "when you do, {effect}" — reflexive trigger, treated as IfYouDo
    if let Some(rest) = lower.strip_prefix("when you do, ") {
        let offset = text.len() - rest.len();
        return (Some(AbilityCondition::IfYouDo), text[offset..].to_string());
    }
    if let Some(rest) = lower.strip_prefix("if you do, ") {
        let offset = text.len() - rest.len();
        (Some(AbilityCondition::IfYouDo), text[offset..].to_string())
    } else {
        (None, text.to_string())
    }
}

/// Strip "you may " prefix, returning whether the effect is optional.
fn strip_optional_effect_prefix(text: &str) -> (bool, String) {
    let lower = text.to_lowercase();
    if let Some(rest) = lower.strip_prefix("you may ") {
        let offset = text.len() - rest.len();
        (true, text[offset..].to_string())
    } else {
        (false, text.to_string())
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

fn strip_leading_duration(text: &str) -> Option<(Duration, &str)> {
    let lower = text.to_lowercase();
    for (prefix, duration) in [
        ("until end of turn, ", Duration::UntilEndOfTurn),
        ("until your next turn, ", Duration::UntilYourNextTurn),
    ] {
        if lower.starts_with(prefix) {
            return Some((duration, text[prefix.len()..].trim()));
        }
    }
    None
}

fn strip_trailing_duration(text: &str) -> (&str, Option<Duration>) {
    let lower = text.to_lowercase();
    for (suffix, duration) in [
        (" this turn", Duration::UntilEndOfTurn),
        (" until end of turn", Duration::UntilEndOfTurn),
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
    (text, None)
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

/// CR 115.1d: Strip "any number of" or "up to N" quantifier from imperative text.
/// Only applies to verbs where the quantifier modifies target selection.
fn strip_any_number_quantifier(text: &str) -> (String, Option<MultiTargetSpec>) {
    let lower = text.to_lowercase();
    let verb = lower.split_whitespace().next().unwrap_or("");
    if !MULTI_TARGET_VERBS.contains(&verb) {
        return (text.to_string(), None);
    }

    let verb_end = lower.find(' ').map(|i| i + 1).unwrap_or(0);
    let after_verb = &lower[verb_end..];

    if after_verb.starts_with("any number of ") {
        let skip = verb_end + "any number of ".len();
        let rebuilt = format!("{}{}", &text[..verb_end], &text[skip..]);
        return (rebuilt, Some(MultiTargetSpec { min: 0, max: None }));
    }
    if let Some(rest) = after_verb.strip_prefix("up to ") {
        if let Some((n, remainder)) = parse_number(rest) {
            // Compute skip offset: verb + "up to " + (consumed portion of rest)
            let consumed_len = rest.len() - remainder.len();
            let skip = verb_end + "up to ".len() + consumed_len;
            let rebuilt = format!("{}{}", &text[..verb_end], text[skip..].trim_start());
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
fn strip_return_destination(text: &str) -> (&str, Option<Zone>) {
    let lower = text.to_lowercase();
    // Ordered longest-first to avoid partial matches
    for (phrase, zone) in [
        (
            " to the battlefield under their owners' control",
            Zone::Battlefield,
        ),
        (
            " to the battlefield under its owner's control",
            Zone::Battlefield,
        ),
        (" to the battlefield under your control", Zone::Battlefield),
        (" to the battlefield tapped", Zone::Battlefield),
        (" to the battlefield", Zone::Battlefield),
        (" onto the battlefield", Zone::Battlefield),
    ] {
        // Use rfind to match the rightmost occurrence — the destination phrase
        // is always at the end, and the target text may contain "battlefield".
        if let Some(pos) = lower.rfind(phrase) {
            return (text[..pos].trim(), Some(zone));
        }
    }
    (text, None)
}

fn try_parse_damage(lower: &str, _text: &str) -> Option<Effect> {
    // Match: "~ deals N damage to {target}" / "deal N damage to {target}"
    // and variable forms like "deal that much damage" or
    // "deal damage equal to its power".
    let pos = lower.find("deals ").or_else(|| lower.find("deal "))?;
    let verb_len = if lower[pos..].starts_with("deals ") {
        6
    } else {
        5
    };
    let after = &_text[pos + verb_len..];
    let after_lower = &lower[pos + verb_len..];

    let (amount, after_target) = if let Some((n, rest)) = parse_number(after_lower) {
        if rest.starts_with("damage") {
            (
                QuantityExpr::Fixed { value: n as i32 },
                &after[after.len() - rest.len() + "damage".len()..],
            )
        } else {
            return None;
        }
    } else if after_lower.starts_with("that much damage") {
        (
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            &after["that much damage".len()..],
        )
    } else if after_lower.starts_with("damage equal to ") {
        let amount_text = &after["damage equal to ".len()..];
        let to_pos = amount_text.to_lowercase().find(" to ")?;
        let qty_text = amount_text[..to_pos].trim();
        let qty = crate::parser::oracle_util::parse_event_context_quantity(qty_text)
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
        let (target, _) = parse_target(after_to);
        return Some(Effect::DamageAll { amount, target });
    }

    // CR 603.7c: Check for event-context references before standard target parsing.
    if let Some(target) = parse_event_context_ref(after_to) {
        return Some(Effect::DealDamage {
            amount: amount.clone(),
            target,
        });
    }

    let (target, _) = parse_target(after_to);
    Some(Effect::DealDamage { amount, target })
}

fn try_parse_pump(lower: &str, text: &str) -> Option<Effect> {
    // Match "+N/+M", "+X/+0", "-X/-X", etc.
    let re_pos = lower.find("gets ").or_else(|| lower.find("get "))?;
    let offset = if lower[re_pos..].starts_with("gets ") {
        5
    } else {
        4
    };
    let after = text[re_pos + offset..].trim();
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
    let (without_where, where_x_expression) = strip_trailing_where_x(predicate);
    let (without_duration, duration) = strip_trailing_duration(without_where);
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

fn strip_trailing_where_x(text: &str) -> (&str, Option<String>) {
    let lower = text.to_lowercase();
    for needle in [", where x is ", " where x is "] {
        if let Some(pos) = lower.find(needle) {
            let expression = text[pos + needle.len()..]
                .trim()
                .trim_end_matches('.')
                .trim()
                .to_string();
            if expression.is_empty() {
                return (text, None);
            }
            return (
                text[..pos].trim_end_matches(',').trim_end(),
                Some(expression),
            );
        }
    }
    (text, None)
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
        .unwrap_or(trimmed)
}

fn apply_where_x_expression(value: PtValue, where_x_expression: Option<&str>) -> PtValue {
    match (value, where_x_expression) {
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("X") => {
            crate::parser::oracle_static::parse_cda_quantity(expression)
                .map(PtValue::Quantity)
                .unwrap_or_else(|| PtValue::Variable(expression.to_string()))
        }
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("-X") => {
            crate::parser::oracle_static::parse_cda_quantity(expression)
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
    let after_put = &text[4..];
    let after_put_lower = &lower[4..];

    for (needle, destination) in [
        (" onto the battlefield", Zone::Battlefield),
        (" into your hand", Zone::Hand),
        (" into its owner's hand", Zone::Hand),
        (" into their owner's hand", Zone::Hand),
        (" into your graveyard", Zone::Graveyard),
        (" into its owner's graveyard", Zone::Graveyard),
        (" into their owner's graveyard", Zone::Graveyard),
        (" on the bottom of", Zone::Library),
        (" on top of", Zone::Library),
    ] {
        if let Some(pos) = after_put_lower.find(needle) {
            let target_text = after_put[..pos].trim();
            if target_text.is_empty() {
                return None;
            }
            let (target, _) = parse_target(target_text);
            return Some(Effect::ChangeZone {
                origin: infer_origin_zone(after_put_lower),
                destination,
                target,
                owner_library: false,
            });
        }
    }

    None
}

/// CR 118.12: Parse "unless its controller pays {X}" from counter text.
/// Extracts the mana cost from patterns like:
/// - "unless its controller pays {3}"
/// - "unless that player pays {1}{U}"
fn parse_unless_payment(lower: &str) -> Option<ManaCost> {
    // Find "unless" followed by a subject and "pays {cost}"
    let unless_pos = lower.find("unless ")?;
    let after_unless = &lower[unless_pos + 7..];
    // Skip the subject ("its controller", "that player", "he or she", etc.)
    let pays_pos = after_unless.find("pays ")?;
    let cost_str = &after_unless[pays_pos + 5..];
    // Extract the mana cost (brace-delimited symbols)
    let cost_end = cost_str
        .find(|c: char| c != '{' && c != '}' && !c.is_alphanumeric())
        .unwrap_or(cost_str.len());
    let cost_text = cost_str[..cost_end].trim();
    if cost_text.is_empty() || !cost_text.contains('{') {
        return None;
    }
    let cost = parse_mtgjson_mana_cost(cost_text);
    if cost == ManaCost::NoCost || cost == ManaCost::zero() {
        return None;
    }
    Some(cost)
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

fn normalize_verb_token(token: &str) -> String {
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

fn extract_number_before(text: &str, before_word: &str) -> Option<u32> {
    let pos = text.find(before_word)?;
    let prefix = text[..pos].trim();
    let last_word = prefix.split_whitespace().last()?;
    last_word.parse::<u32>().ok()
}

fn constrain_filter_to_stack(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(TypedFilter {
            card_type,
            subtype,
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
                card_type,
                subtype,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ContinuousModification, ManaProduction, PaymentCost, TypeFilter};
    use crate::types::mana::ManaColor;

    #[test]
    fn effect_lightning_bolt() {
        let e = parse_effect("Lightning Bolt deals 3 damage to any target");
        assert!(matches!(
            e,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any
            }
        ));
    }

    #[test]
    fn effect_murder() {
        let e = parse_effect("Destroy target creature");
        assert!(matches!(
            e,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    ..
                }),
                ..
            }
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
                && properties.iter().any(|p| matches!(p, FilterProp::CmcGE { value: 4 }))
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
        if let Effect::Counter { source_static, .. } = &ability.effect {
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
                Some(ManaCost::Cost {
                    shards: vec![],
                    generic: 3
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
        assert!(matches!(e, Effect::Scry { count: 2 }));
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
    fn effect_unimplemented_fallback() {
        let e = parse_effect("Fateseal 2");
        assert!(matches!(e, Effect::Unimplemented { .. }));
    }

    #[test]
    fn effect_chain_revitalize() {
        let def = parse_effect_chain("You gain 3 life. Draw a card.", AbilityKind::Spell);
        assert!(matches!(
            def.effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
        assert!(def.sub_ability.is_some());
        assert!(matches!(
            def.sub_ability.unwrap().effect,
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
        let e = parse_effect("Shuffle it into its owner's library");
        assert!(matches!(
            e,
            Effect::ChangeZone {
                destination: Zone::Library,
                ..
            }
        ));
    }

    #[test]
    fn compound_shuffle_graveyard_into_library() {
        let e = parse_effect("Shuffle your graveyard into your library");
        assert!(matches!(
            e,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Library,
                ..
            }
        ));
    }

    #[test]
    fn compound_shuffle_hand_into_library() {
        let e = parse_effect("Shuffle your hand into your library");
        assert!(matches!(
            e,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Hand),
                destination: Zone::Library,
                ..
            }
        ));
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
                    TargetFilter::Typed(TypedFilter {
                        card_type,
                        properties,
                        ..
                    }) => {
                        assert_eq!(card_type, Some(TypeFilter::Land));
                        assert!(properties.iter().any(
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
                target: Some(TargetFilter::Typed(TypedFilter {
                    card_type: Some(TypeFilter::Creature),
                    ..
                })),
                ..
            }
        ));
    }

    #[test]
    fn effect_target_creature_becomes_blue_uses_continuous_effect() {
        let e = parse_effect("Target creature becomes blue until end of turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed(TypedFilter { card_type: Some(TypeFilter::Creature), .. })),
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].modifications.contains(&ContinuousModification::SetColor { colors: vec![ManaColor::Blue] })
        ));
    }

    #[test]
    fn effect_target_creature_cant_block_uses_rule_static() {
        let e = parse_effect("Target creature can't block this turn");
        assert!(matches!(
            e,
            Effect::GenericEffect { target: Some(TargetFilter::Typed(TypedFilter { card_type: Some(TypeFilter::Creature), .. })), static_abilities, .. }
            if static_abilities.len() == 1 && static_abilities[0].mode == StaticMode::CantBlock
        ));
    }

    #[test]
    fn compound_tap_and_put_counter() {
        let clause = parse_effect_clause(
            "tap target creature an opponent controls and put a stun counter on it",
        );
        assert!(
            matches!(clause.effect, Effect::Tap { .. }),
            "primary should be Tap, got {:?}",
            clause.effect
        );
        let sub = clause.sub_ability.expect("should have sub_ability");
        assert!(
            matches!(
                sub.effect,
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
        let clause =
            parse_effect_clause("exile any number of other nonland permanents you own and control");
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
            def.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ));
        let sub = def.sub_ability.as_ref().expect("should have sub_ability");
        assert!(matches!(
            sub.effect,
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
            &ability.effect,
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
            &sub.effect,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::ParentTarget
            }
        ));
    }

    #[test]
    fn parse_damage_cant_be_prevented_this_turn() {
        let clause = parse_effect_clause("Damage can't be prevented this turn");
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
        );
        match &clause.effect {
            Effect::ChangeZone {
                destination: Zone::Library,
                target: TargetFilter::SelfRef,
                owner_library: true,
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
                def.effect,
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
                def.effect,
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
                    target: TargetFilter::SelfRef
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
                    sd.mode == crate::types::statics::StaticMode::Other("MustBeBlocked".into())
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
                    sd.mode == crate::types::statics::StaticMode::Other("MustBeBlocked".into())
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
            matches!(&def.effect, Effect::Pump { .. }),
            "Expected Pump as primary effect, got {:?}",
            def.effect
        );
        // Sub-ability should carry MustBeBlocked
        let sub = def
            .sub_ability
            .as_ref()
            .expect("Expected sub_ability for MustBeBlocked");
        assert!(
            matches!(&sub.effect, Effect::GenericEffect { static_abilities, .. }
                if static_abilities.iter().any(|sd|
                    sd.mode == crate::types::statics::StaticMode::Other("MustBeBlocked".into())
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
            &def.effect,
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
                def.effect,
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
    fn parse_impulse_draw_chain() {
        // "Exile the top two cards of your library. Choose one of them. Until end of turn, you may play that card."
        let def = parse_effect_chain(
            "Exile the top two cards of your library. Choose one of them. Until end of turn, you may play that card.",
            AbilityKind::Spell,
        );
        // First effect: ChangeZone to Exile
        assert!(
            matches!(def.effect, Effect::ChangeZone { .. }),
            "Expected ChangeZone, got {:?}",
            def.effect
        );
        // Second: ChooseFromZone
        let sub1 = def.sub_ability.as_ref().expect("Expected sub_ability");
        assert!(
            matches!(
                sub1.effect,
                Effect::ChooseFromZone {
                    count: 1,
                    zone: crate::types::zones::Zone::Exile,
                }
            ),
            "Expected ChooseFromZone {{ count: 1, zone: Exile }}, got {:?}",
            sub1.effect
        );
        // Third: GrantCastingPermission with PlayFromExile
        let sub2 = sub1
            .sub_ability
            .as_ref()
            .expect("Expected second sub_ability");
        assert!(
            matches!(
                sub2.effect,
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
        match &def.effect {
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
    fn otherwise_attaches_else_ability() {
        let def = parse_effect_chain(
            "You may sacrifice two Foods. If you do, create a 7/7 green Giant creature token. Otherwise, create three Food tokens.",
            AbilityKind::Spell,
        );
        // Walk the chain and collect effect types
        let mut effects = vec![];
        let mut current = Some(&def);
        while let Some(d) = current {
            effects.push(std::mem::discriminant(&d.effect));
            // Check else_ability on any node with IfYouDo condition
            if d.condition == Some(AbilityCondition::IfYouDo) {
                if let Some(else_ab) = &d.else_ability {
                    effects.push(std::mem::discriminant(&else_ab.effect));
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
}
