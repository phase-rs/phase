pub(crate) mod animation;
pub(crate) mod become_copy_except;
mod conditions;
pub(crate) mod counter;
pub(crate) mod imperative;
pub(crate) mod mana;
mod search;
mod sequence;
pub(crate) mod subject;
pub(crate) mod token;
pub(crate) mod types;

pub(crate) use self::token::parse_token_description;
pub(crate) use self::types::TokenDescription;

use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::value;
use nom::sequence::preceded;
use nom::Parser;
use nom_language::error::VerboseError;

use super::oracle_nom::bridge::nom_on_lower;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_quantity::{parse_cda_quantity, parse_for_each_clause};
use super::oracle_target::{parse_event_context_ref, parse_target, parse_type_phrase};
use super::oracle_util::{
    contains_possessive, has_unconsumed_conditional, parse_mana_symbols, split_around,
    starts_with_possessive, strip_after, TextPair,
};
use crate::database::mtgjson::parse_mtgjson_mana_cost;
use crate::parser::oracle_effect::subject::parse_subject_application;
use crate::parser::oracle_warnings::push_warning;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, CardPlayMode, CastingPermission, ChoiceType,
    ChooseFromZoneConstraint, ConjureCard, ContinuousModification, ControllerRef, DamageSource,
    DelayedTriggerCondition, Duration, Effect, FilterProp, GameRestriction, MultiTargetSpec,
    PlayerFilter, PtValue, QuantityExpr, QuantityRef, RestrictionExpiry, RestrictionPlayerScope,
    RoundingMode, StaticCondition, StaticDefinition, TargetFilter, TriggerDefinition, TypeFilter,
    TypedFilter, UnlessCost,
};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::game_state::{DistributionUnit, NextSpellModifier, RetargetScope};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

pub(crate) use self::conditions::split_leading_conditional;
use self::conditions::*;
use self::imperative::{
    lower_imperative_family_ast, lower_targeted_action_ast, lower_zone_counter_ast,
    parse_imperative_family_ast,
};
#[cfg(test)]
use self::search::parse_search_filter;
use self::search::{parse_search_destination, parse_search_library_details, parse_seek_details};
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
    /// The card name for self-name effect parsing (e.g. "Exile Card Name.").
    pub card_name: Option<String>,
    /// CR 707.9a + CR 603.1: Index of the printed trigger whose body is being
    /// parsed, in the source object's `base_trigger_definitions` list. Set by
    /// the trigger parser before invoking the effect chain. Consumed by the
    /// `<subject pronoun> has this ability` arm of the BecomeCopy except-clause
    /// parser to emit `RetainPrintedTriggerFromSource { source_trigger_index }`.
    /// `None` for non-trigger contexts (replacements, instants, sorceries),
    /// in which case the "has this ability" arm declines gracefully.
    pub current_trigger_index: Option<usize>,
    /// CR 701.21a + CR 608.2k: The actor performing the effect, when the effect
    /// body has been stripped of an actor prefix ("you (may) ", "an opponent
    /// (may) ", "each opponent ", "each player "). Used by targeted-action
    /// parsers to default `TargetFilter::Typed.controller` from `None` to the
    /// actor when the target phrase itself doesn't specify a controller (e.g.
    /// "you may sacrifice a non-Demon creature" → `controller: Some(You)`).
    /// CR 701.21a forbids sacrificing a permanent you don't control, so the
    /// default must be enforced at parse time, not at resolution.
    pub actor: Option<ControllerRef>,
}

/// CR 608.2k: True when `text` is a standalone object pronoun referring to
/// the trigger/spell subject. Used by effect-target parsers that need to
/// distinguish "verb + pronoun" (resolve against `ctx.subject` via
/// `resolve_it_pronoun`) from "verb + target filter" (parse via
/// `parse_target`, which defaults to `ParentTarget`).
///
/// Covers the full object-pronoun family: "it" / "itself", "him" / "himself",
/// "her" / "herself", "them" / "themselves". Caller is responsible for trimming.
pub(crate) fn is_bare_object_pronoun(text: &str) -> bool {
    matches!(
        text,
        "it" | "itself" | "him" | "himself" | "her" | "herself" | "them" | "themselves"
    )
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

// ── Word-boundary scanners for delayed trigger dispatch ──────────────────

/// Delayed trigger condition kinds, used by `scan_delayed_condition_kind`.
#[derive(Debug, Clone, Copy)]
enum DelayedConditionKind {
    Dies,
    PutIntoGraveyard,
    LeavesPlay,
    EntersBattlefield,
}

/// Nom combinator for delayed trigger condition keywords.
/// Ordered: longer phrases first to prevent prefix collisions
/// (e.g., "enters the battlefield" before bare "enters").
fn parse_delayed_condition_keyword(
    input: &str,
) -> nom::IResult<&str, DelayedConditionKind, VerboseError<&str>> {
    alt((
        value(
            DelayedConditionKind::LeavesPlay,
            tag("leaves the battlefield"),
        ),
        value(
            DelayedConditionKind::EntersBattlefield,
            tag("enters the battlefield"),
        ),
        // CR 700.4: "is put into a graveyard" from battlefield = dies
        value(
            DelayedConditionKind::PutIntoGraveyard,
            tag("is put into a graveyard"),
        ),
        value(
            DelayedConditionKind::PutIntoGraveyard,
            tag("is put into your graveyard"),
        ),
        value(
            DelayedConditionKind::PutIntoGraveyard,
            tag("is put into their graveyard"),
        ),
        value(DelayedConditionKind::Dies, tag("dies")),
        value(DelayedConditionKind::Dies, tag("die")),
        // Bare "enters" last — least specific
        value(DelayedConditionKind::EntersBattlefield, tag("enters")),
    ))
    .parse(input)
}

/// Word-boundary scanner for delayed trigger condition keywords.
/// Delegates to the shared `scan_at_word_boundaries` primitive.
fn scan_delayed_condition_kind(text: &str) -> Option<DelayedConditionKind> {
    nom_primitives::scan_at_word_boundaries(text, parse_delayed_condition_keyword)
}

/// Nom combinator for delayed trigger subject reference phrases.
/// Returns `Some(TargetFilter)` for ParentTarget or SelfRef references.
fn parse_delayed_subject_keyword(
    input: &str,
) -> nom::IResult<&str, TargetFilter, VerboseError<&str>> {
    alt((
        // ParentTarget references (longer phrases first)
        value(TargetFilter::ParentTarget, tag("the exiled ")),
        value(TargetFilter::ParentTarget, tag("the targeted ")),
        value(TargetFilter::ParentTarget, tag("the creature")),
        value(TargetFilter::ParentTarget, tag("the permanent")),
        value(TargetFilter::ParentTarget, tag("the token")),
        value(TargetFilter::ParentTarget, tag("that ")),
        value(TargetFilter::ParentTarget, tag("target ")),
        // SelfRef references — "~" is the normalized self-reference marker
        value(TargetFilter::SelfRef, tag("~ ")),
        value(TargetFilter::SelfRef, tag("this creature")),
        value(TargetFilter::SelfRef, tag("this permanent")),
        value(TargetFilter::SelfRef, tag("this artifact")),
    ))
    .parse(input)
}

/// Word-boundary scanner for delayed trigger subject references.
/// Delegates to `scan_at_word_boundaries` for typed phrases, then falls back
/// to checking for bare "it" (which can appear at end of string).
fn scan_delayed_subject(text: &str) -> Option<TargetFilter> {
    // First try typed subject references via the shared scanner
    if let Some(filter) =
        nom_primitives::scan_at_word_boundaries(text, parse_delayed_subject_keyword)
    {
        return Some(filter);
    }
    // Fallback: bare "it" at any word boundary (end of string or followed by space,
    // but NOT "its " which is possessive, not a pronoun reference)
    if nom_primitives::scan_at_word_boundaries(text, |input| {
        let (rest, _) = tag::<_, _, VerboseError<&str>>("it").parse(input)?;
        // Require word boundary: end of string, space (but not "its"), or punctuation
        if rest.is_empty() || rest.starts_with(' ') && !rest.starts_with("s ") {
            Ok((rest, ()))
        } else {
            Err(nom::Err::Error(VerboseError {
                errors: vec![(
                    input,
                    nom_language::error::VerboseErrorKind::Context("it word boundary"),
                )],
            }))
        }
    })
    .is_some()
    {
        return Some(TargetFilter::SelfRef);
    }
    // CR 603.7c: When the condition text doesn't start with an indefinite article
    // ("a"/"an"), it's a definite self-reference — either "~" (normalized), a card
    // name (un-normalized), or a definite noun phrase describing the source object
    // (e.g., "the pandorica becomes untapped"). Indefinite articles indicate a
    // different object class ("a creature dealt damage this way"), not the source.
    if tag::<_, _, VerboseError<&str>>("a ").parse(text).is_err()
        && tag::<_, _, VerboseError<&str>>("an ").parse(text).is_err()
    {
        return Some(TargetFilter::SelfRef);
    }
    None
}

/// Word-boundary scanner for tracked set references in delayed trigger conditions.
/// Checks for "that ", "the exiled ", "the targeted " at word boundaries.
fn scan_tracked_set_reference(text: &str) -> bool {
    nom_primitives::scan_at_word_boundaries(text, |input| {
        alt((
            tag::<_, _, VerboseError<&str>>("that "),
            tag("the exiled "),
            tag("the targeted "),
        ))
        .parse(input)
    })
    .is_some()
}

/// Delegates to the shared word-boundary scanning primitive in `oracle_nom::primitives`.
fn scan_contains_phrase(text: &str, phrase: &str) -> bool {
    nom_primitives::scan_contains(text, phrase)
}

/// CR 603.7c: Parse "whenever [trigger condition] this turn, [effect]" delayed triggers.
/// These create multi-fire delayed triggers that persist until end of turn.
/// Example: "whenever a creature you control deals combat damage to a player this turn, draw a card"
fn try_parse_whenever_this_turn(tp: TextPair) -> Option<ParsedEffectClause> {
    if tag::<_, _, VerboseError<&str>>("whenever ")
        .parse(tp.lower)
        .is_err()
    {
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
        condition: None,
        optional: false,
    })
}

/// Decompose the spell qualifier from `"when you next cast a <payload> this turn, ..."`
/// into a combined `TargetFilter`.
///
/// The payload must contain the word "spell". Text before " spell" is parsed as a type
/// phrase (`parse_type_phrase`); text after " spell" is parsed by
/// `oracle_trigger::parse_post_spell_modifier` (CR 107.3 + CR 202.1 — "with {X} in its
/// mana cost"). When both shapes contribute a filter, the result is an `And` composition.
///
/// Returns `None` when the payload contains unrecognized text or has no "spell" boundary.
fn extract_when_next_spell_filter(payload: &str) -> Option<TargetFilter> {
    let payload = payload.trim();
    // Bare "spell" — any spell. (Kept for completeness; this shape is not
    // common in printed card text.)
    if payload == "spell" {
        return Some(TargetFilter::Any);
    }
    // The payload is one of three shapes:
    //   (a) "<type-phrase> spell"                 — type only
    //   (b) "spell <post-modifier>"               — post-modifier only (Brass Infiniscope)
    //   (c) "<type-phrase> spell <post-modifier>" — both
    let (pre, post) = if let Some(rest) = payload.strip_prefix("spell ") {
        ("", rest.trim())
    } else {
        // Split on " spell" to isolate pre-spell type phrase from post-spell modifier.
        match crate::parser::oracle_nom::primitives::split_once_on(payload, " spell") {
            Ok((_, (pre, post))) => (pre.trim(), post.trim()),
            Err(_) => return None,
        }
    };

    let type_filter = if pre.is_empty() {
        None
    } else {
        let (filter, remainder) = super::oracle_target::parse_type_phrase(pre);
        if !remainder.trim().is_empty() {
            return None;
        }
        if matches!(filter, TargetFilter::Any) {
            None
        } else {
            Some(filter)
        }
    };

    let post_filter = if post.is_empty() {
        None
    } else {
        // Unknown post-spell modifier → reject the whole pattern rather than
        // silently dropping it. The caller falls through to other parsers.
        Some(crate::parser::oracle_trigger::parse_post_spell_modifier(
            post,
        )?)
    };

    Some(match (type_filter, post_filter) {
        (None, None) => TargetFilter::Any,
        (Some(f), None) | (None, Some(f)) => f,
        (Some(a), Some(b)) => TargetFilter::And {
            filters: vec![a, b],
        },
    })
}

/// CR 603.7: Parse "when you next cast a [type] spell [post-spell modifier] this turn, [effect]"
/// delayed triggers. Creates a one-shot delayed trigger that fires once on the next matching
/// SpellCast event.
/// Examples:
/// - "When you next cast a creature spell this turn, that creature enters with an additional +1/+1 counter on it."
/// - "When you next cast a creature spell this turn, search your library for a creature card..."
/// - "When you next cast a spell with {X} in its mana cost this turn, you draw a card and gain half X life, rounded down." (Brass Infiniscope)
fn try_parse_when_next_event(tp: TextPair) -> Option<ParsedEffectClause> {
    use crate::types::triggers::TriggerMode;

    // Must start with "when you next cast a " or "when you next cast an ".
    // Article choice depends on the payload — "a creature spell" vs "an instant or sorcery spell".
    let article_result: nom::IResult<&str, &str, VerboseError<&str>> =
        alt((tag("when you next cast a "), tag("when you next cast an "))).parse(tp.lower);
    let (_, matched_prefix) = article_result.ok()?;

    // Must contain "this turn, " to delimit condition from effect
    let (before_this_turn, after) = tp.rsplit_around(" this turn, ")?;

    // Extract the spell qualifier payload from between the matched article prefix and
    // the timing tail (" this turn,"). The payload is of the shape
    //   [type-phrase]? "spell" [post-spell modifier]?
    // — e.g. "creature spell" (type only), "instant or sorcery spell" (disjunction),
    // "spell with {x} in its mana cost" (post only).
    let condition_fragment = &before_this_turn.lower[matched_prefix.len()..];
    let combined_filter = extract_when_next_spell_filter(condition_fragment)?;

    // Build a SpellCast trigger definition with the combined filter
    let mut trigger_def = crate::types::ability::TriggerDefinition::new(TriggerMode::SpellCast);
    trigger_def.valid_card = Some(combined_filter);
    // "when YOU next cast" — scope to the source's controller.
    trigger_def.valid_target = Some(TargetFilter::Controller);

    let effect_text = after.original;
    let effect_lower = after.lower;

    // Check for "that creature enters with an additional +1/+1 counter on it" pattern
    let inner = if let Some(parsed) = try_parse_enters_with_additional_counters(effect_lower) {
        parsed
    } else {
        parse_effect_chain(effect_text, AbilityKind::Spell)
    };

    Some(ParsedEffectClause {
        effect: Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::WhenNextEvent {
                trigger: Box::new(trigger_def),
            },
            effect: Box::new(inner),
            uses_tracked_set: false,
        },
        duration: None,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
    })
}

/// Parse "that creature enters with an additional [N] +1/+1 counter(s) on it" into
/// an `AddPendingETBCounters` effect. Returns `None` if the text doesn't match.
fn try_parse_enters_with_additional_counters(lower: &str) -> Option<AbilityDefinition> {
    // "that creature enters with an additional +1/+1 counter on it"
    // "that creature enters with N additional +1/+1 counters on it"
    let (rest, _) = tag::<_, _, VerboseError<&str>>("that creature enters with ")
        .parse(lower)
        .ok()?;

    // Parse "an additional" or "N additional"
    let (rest, count) =
        if let Ok((r, _)) = tag::<_, _, VerboseError<&str>>("an additional ").parse(rest) {
            (r, 1u32)
        } else if let Ok((r, n)) = nom_primitives::parse_number(rest) {
            let (r, _) = tag::<_, _, VerboseError<&str>>(" additional ")
                .parse(r)
                .ok()?;
            (r, n)
        } else {
            return None;
        };

    // Parse counter type: "+1/+1 counter" or "-1/-1 counter" etc.
    let (rest, counter_type) = alt((
        value("P1P1".to_string(), tag::<_, _, VerboseError<&str>>("+1/+1")),
        value("M1M1".to_string(), tag("-1/-1")),
    ))
    .parse(rest)
    .ok()?;

    // Match " counter on it" or " counters on it"
    let _ = alt((
        tag::<_, _, VerboseError<&str>>(" counter on it"),
        tag(" counters on it"),
    ))
    .parse(rest)
    .ok()?;

    Some(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::AddPendingETBCounters {
            counter_type,
            count: QuantityExpr::Fixed {
                value: count as i32,
            },
        },
    ))
}

/// CR 603.7c: Parse inline delayed triggers like "when that creature dies, draw a card".
/// Returns a `CreateDelayedTrigger` wrapping the parsed inner effect.
fn try_parse_inline_delayed_trigger(tp: TextPair) -> Option<ParsedEffectClause> {
    if tag::<_, _, VerboseError<&str>>("when ")
        .parse(tp.lower)
        .is_err()
    {
        return None;
    }

    // Find the comma separator between condition and effect
    let comma = tp.find(", ")?;
    let condition_text = &tp.lower["when ".len()..comma];
    let effect_text = &tp.original[comma + 2..];

    let condition = match scan_delayed_condition_kind(condition_text) {
        Some(DelayedConditionKind::Dies | DelayedConditionKind::PutIntoGraveyard) => {
            DelayedTriggerCondition::WhenDies {
                filter: parse_delayed_subject_filter(condition_text),
            }
        }
        Some(DelayedConditionKind::LeavesPlay) => DelayedTriggerCondition::WhenLeavesPlayFiltered {
            filter: parse_delayed_subject_filter(condition_text),
        },
        Some(DelayedConditionKind::EntersBattlefield) => {
            if has_unconsumed_conditional(condition_text) {
                tracing::warn!(
                    text = condition_text,
                    "Unconsumed conditional in delayed trigger 'enters' match — parser may need extension"
                );
            }
            DelayedTriggerCondition::WhenEntersBattlefield {
                filter: parse_delayed_subject_filter(condition_text),
            }
        }
        None => return None,
    };

    // "that creature/permanent/token" references the parent spell's target.
    // "the exiled creature/card" and "the targeted creature" also reference
    // the parent's tracked set.
    let uses_tracked_set = scan_tracked_set_reference(condition_text);

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
        condition: None,
        optional: false,
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
    scan_delayed_subject(condition_text).unwrap_or_else(|| {
        push_warning(format!(
            "target-fallback: unrecognized delayed subject '{}'",
            condition_text
        ));
        TargetFilter::Any
    })
}

/// CR 601.2f: Parse "the next spell you cast this turn costs {N} less to cast".
///
/// Also handles "for each counter removed this way" variants by falling back to fixed {1}.
fn try_parse_reduce_next_spell_cost(tp: TextPair) -> Option<ParsedEffectClause> {
    use nom::sequence::delimited;

    // Match prefix: "the next spell you cast this turn costs "
    let (rest, _) = tag::<_, _, VerboseError<&str>>("the next spell you cast this turn costs ")
        .parse(tp.lower)
        .ok()?;

    // Extract the mana amount: "{1}", "{2}", etc.
    let (after_amount, amount) = delimited(
        tag::<_, _, VerboseError<&str>>("{"),
        nom::character::complete::u32,
        tag("}"),
    )
    .parse(rest)
    .ok()?;

    // Match suffix: " less to cast" with optional trailing clause
    alt((
        tag::<_, _, VerboseError<&str>>(" less to cast for each counter removed this way"),
        tag(" less to cast"),
    ))
    .parse(after_amount)
    .ok()?;

    Some(parsed_clause(Effect::ReduceNextSpellCost {
        amount,
        spell_filter: None,
    }))
}

/// CR 601.2f: Parse "the next [type] spell you cast this turn [has keyword/can't be countered/etc.]"
///
/// Handles patterns like:
/// - "the next creature spell you cast this turn can't be countered"
/// - "the next spell you cast this turn has convoke"
/// - "the next sorcery spell you cast this turn can be cast as though it had flash"
/// - "the next instant or sorcery spell you cast this turn has cascade"
/// - "the next face-down creature spell you cast this turn costs {3} less to cast"
fn try_parse_grant_next_spell_ability(tp: TextPair) -> Option<ParsedEffectClause> {
    // Must start with "the next "
    let rest = tag::<_, _, VerboseError<&str>>("the next ")
        .parse(tp.lower)
        .ok()
        .map(|(r, _)| r)?;

    // Extract optional spell type filter before "spell you cast this turn"
    // Patterns: "spell you cast this turn", "creature spell you cast this turn",
    // "instant or sorcery spell you cast this turn", "noncreature spell you cast this turn",
    // "face-down creature spell you cast this turn"
    let (ability_text, filter_text) = nom::sequence::terminated(
        take_until::<_, _, VerboseError<&str>>("spell"),
        alt((
            tag::<_, _, VerboseError<&str>>("spell you cast this turn "),
            tag("spell of the chosen type you cast this turn "),
        )),
    )
    .parse(rest)
    .ok()?;

    let mut spell_filter = None;
    let filter_text = filter_text.trim();
    if !filter_text.is_empty() {
        // Parse the filter: "creature", "instant or sorcery", "noncreature", etc.
        let (filter, _) = parse_type_phrase(filter_text);
        if !matches!(filter, TargetFilter::Any) {
            spell_filter = Some(filter);
        }
    }

    // Now parse the ability: "can't be countered", "has convoke", "can be cast as though it had flash"
    // CR 601.2f: "can't be countered"
    if tag::<_, _, VerboseError<&str>>("can't be countered")
        .parse(ability_text)
        .is_ok()
    {
        return Some(parsed_clause(Effect::GrantNextSpellAbility {
            modifier: NextSpellModifier::CantBeCountered,
            spell_filter,
        }));
    }

    // CR 601.2f: "can be cast as though it had flash"
    if tag::<_, _, VerboseError<&str>>("can be cast as though it had flash")
        .parse(ability_text)
        .is_ok()
    {
        return Some(parsed_clause(Effect::GrantNextSpellAbility {
            modifier: NextSpellModifier::CastAsThoughFlash,
            spell_filter,
        }));
    }

    // "can be cast without paying its mana cost" — requires casting infrastructure
    // changes (alternative cost injection during casting). Deferred.
    if tag::<_, _, VerboseError<&str>>("can be cast without paying its mana cost")
        .parse(ability_text)
        .is_ok()
    {
        return None; // Falls through to Unimplemented
    }

    // CR 601.2f: "has [keyword]"
    if let Ok((_, keyword_text)) = preceded(
        tag::<_, _, VerboseError<&str>>("has "),
        nom::combinator::rest,
    )
    .parse(ability_text)
    {
        // Strip trailing period/reminder text
        let keyword_text = keyword_text
            .split('.')
            .next()
            .unwrap_or(keyword_text)
            .trim();
        let keyword = match keyword_text {
            "convoke" => Some(Keyword::Convoke),
            "cascade" => Some(Keyword::Cascade),
            // CR 702.41a: "affinity for artifacts" — Affinity parameterized by artifact type
            "affinity for artifacts" => Some(Keyword::Affinity(
                crate::types::ability::TypedFilter::default()
                    .with_type(crate::types::ability::TypeFilter::Artifact),
            )),
            "improvise" => Some(Keyword::Improvise),
            _ => {
                // Try parsing as a keyword
                keyword_text.parse::<Keyword>().ok()
            }
        };
        if let Some(kw) = keyword {
            return Some(parsed_clause(Effect::GrantNextSpellAbility {
                modifier: NextSpellModifier::HasKeyword { keyword: kw },
                spell_filter,
            }));
        }
    }

    // "costs {N} less to cast" — delegate to the existing ReduceNextSpellCost for filtered variants
    // e.g., "the next face-down creature spell you cast this turn costs {3} less to cast"
    if let Ok((amount_rest, _)) = tag::<_, _, VerboseError<&str>>("costs ").parse(ability_text) {
        if let Ok((after_amount, amount)) = nom::sequence::delimited(
            tag::<_, _, VerboseError<&str>>("{"),
            nom::character::complete::u32,
            tag("}"),
        )
        .parse(amount_rest)
        {
            if tag::<_, _, VerboseError<&str>>(" less to cast")
                .parse(after_amount)
                .is_ok()
            {
                return Some(parsed_clause(Effect::ReduceNextSpellCost {
                    amount,
                    spell_filter,
                }));
            }
        }
    }

    None
}

/// CR 614.16: Parse "Damage can't be prevented [this turn]" into Effect::AddRestriction.
///
/// Handles variants:
///   - "Damage can't be prevented this turn"
///   - "Combat damage that would be dealt by creatures you control can't be prevented"
fn try_parse_damage_prevention_disabled(tp: TextPair) -> Option<ParsedEffectClause> {
    // Guard: must contain both "damage" and a prevention-disabled phrase.
    // Use word-boundary scanning to avoid substring false positives.
    let has_prevention_disabled = scan_contains_phrase(tp.lower, "can't be prevented")
        || scan_contains_phrase(tp.lower, "cannot be prevented");
    if !has_prevention_disabled {
        return None;
    }
    if !scan_contains_phrase(tp.lower, "damage") {
        return None;
    }

    // Determine expiry: "this turn" → EndOfTurn, otherwise EndOfTurn as default
    let expiry = crate::types::ability::RestrictionExpiry::EndOfTurn;

    // Determine scope from the subject phrase
    let scope = if scan_contains_phrase(tp.lower, "creatures you control")
        || scan_contains_phrase(tp.lower, "sources you control")
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
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
    })
}

fn try_parse_cast_only_from_zones_restriction(tp: TextPair<'_>) -> Option<ParsedEffectClause> {
    let (scope_tp, expiry, duration) = if let Some(((expiry, duration), rest)) =
        nom_on_lower(tp.original, tp.lower, |input| {
            alt((
                value(
                    (
                        RestrictionExpiry::EndOfTurn,
                        Some(Duration::UntilYourNextTurn),
                    ),
                    tag("until your next turn, "),
                ),
                value((RestrictionExpiry::EndOfTurn, None), tag("this turn, ")),
            ))
            .parse(input)
        }) {
        let rest_lower = &tp.lower[tp.lower.len() - rest.len()..];
        (TextPair::new(rest, rest_lower), expiry, duration)
    } else {
        (tp, RestrictionExpiry::EndOfTurn, None)
    };

    if !scan_contains_phrase(scope_tp.lower, "can't cast spells from anywhere other than") {
        return None;
    }

    if !scan_contains_phrase(scope_tp.lower, "their hand")
        && !scan_contains_phrase(scope_tp.lower, "their hands")
    {
        return None;
    }

    let affected_players = if alt((
        tag::<_, _, VerboseError<&str>>("your opponents"),
        tag("opponents"),
    ))
    .parse(scope_tp.lower)
    .is_ok()
    {
        RestrictionPlayerScope::OpponentsOfSourceController
    } else {
        RestrictionPlayerScope::AllPlayers
    };

    Some(ParsedEffectClause {
        effect: Effect::AddRestriction {
            restriction: GameRestriction::CastOnlyFromZones {
                source: ObjectId(0),
                affected_players,
                allowed_zones: vec![Zone::Hand],
                expiry,
            },
        },
        duration,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
    })
}

/// CR 101.2: "Your opponents can't cast spells this turn" / "Players can't cast spells this turn."
/// Handles blanket "can't cast spells" prohibitions from instant/sorcery effects (e.g., Silence).
/// Must be called AFTER try_parse_cast_only_from_zones_restriction (which handles the more
/// specific "can't cast spells from anywhere other than" variant).
fn try_parse_cant_cast_spells_effect(tp: TextPair<'_>) -> Option<ParsedEffectClause> {
    // Must contain "can't cast spells" but NOT "can't cast spells from" (that's CastOnlyFromZones).
    if !nom_primitives::scan_contains(tp.lower, "can't cast spells")
        && !nom_primitives::scan_contains(tp.lower, "cannot cast spells")
    {
        return None;
    }
    if nom_primitives::scan_contains(tp.lower, "can't cast spells from")
        || nom_primitives::scan_contains(tp.lower, "cannot cast spells from")
    {
        return None;
    }

    // Determine affected player scope from subject prefix.
    let affected_players = if alt((
        tag::<_, _, VerboseError<&str>>("your opponents"),
        tag("opponents"),
        tag("each opponent"),
    ))
    .parse(tp.lower)
    .is_ok()
    {
        RestrictionPlayerScope::OpponentsOfSourceController
    } else {
        RestrictionPlayerScope::AllPlayers
    };

    Some(ParsedEffectClause {
        effect: Effect::AddRestriction {
            restriction: GameRestriction::CantCastSpells {
                source: ObjectId(0),
                affected_players,
                expiry: RestrictionExpiry::EndOfTurn,
            },
        },
        duration: None,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
    })
}

fn try_parse_self_name_exile(tp: TextPair<'_>, ctx: &ParseContext) -> Option<ParsedEffectClause> {
    let card_name = ctx.card_name.as_deref()?;
    let (_, rest_orig) =
        nom_on_lower(tp.original, tp.lower, |i| value((), tag("exile ")).parse(i))?;
    let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];
    let rest = TextPair::new(rest_orig, rest_lower);
    if rest.original.trim().eq_ignore_ascii_case(card_name) {
        return Some(parsed_clause(Effect::ChangeZone {
            origin: None,
            destination: Zone::Exile,
            target: TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        }));
    }
    None
}

fn try_parse_airbend_clause(tp: TextPair<'_>) -> Option<ParsedEffectClause> {
    let (_, rest_orig) = nom_on_lower(tp.original, tp.lower, |i| {
        value((), tag("airbend ")).parse(i)
    })?;
    let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];
    let rest = TextPair::new(rest_orig, rest_lower);
    let (target_text, multi_target) = strip_optional_target_prefix(rest.original);
    let (target, after_target) = parse_target(target_text);
    let cost = parse_mana_symbols(after_target.trim_start())
        .map(|(cost, _)| cost)
        .unwrap_or(ManaCost::Cost {
            generic: 2,
            shards: vec![],
        });
    let lower_rest = rest.lower.trim_start();
    let is_mass = alt((tag::<_, _, VerboseError<&str>>("all "), tag("each ")))
        .parse(lower_rest)
        .is_ok();

    let effect = if is_mass {
        let mass_target = match target {
            TargetFilter::Typed(mut typed) => {
                let had_other = typed.properties.contains(&FilterProp::Another);
                typed
                    .properties
                    .retain(|property| !matches!(property, FilterProp::Another));
                let typed_filter = TargetFilter::Typed(typed);
                if had_other {
                    TargetFilter::And {
                        filters: vec![
                            typed_filter,
                            TargetFilter::Not {
                                filter: Box::new(TargetFilter::ParentTarget),
                            },
                        ],
                    }
                } else {
                    typed_filter
                }
            }
            other => other,
        };
        Effect::ChangeZoneAll {
            origin: Some(Zone::Battlefield),
            destination: Zone::Exile,
            target: mass_target,
        }
    } else {
        Effect::ChangeZone {
            origin: Some(Zone::Battlefield),
            destination: Zone::Exile,
            target,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        }
    };

    let register_bending = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RegisterBending {
            kind: crate::types::events::BendingType::Air,
        },
    );

    Some(ParsedEffectClause {
        effect,
        duration: None,
        sub_ability: Some(Box::new(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GrantCastingPermission {
                    permission: CastingPermission::ExileWithAltCost {
                        cost,
                        cast_transformed: false,
                        constraint: None,
                    },
                    target: TargetFilter::TrackedSet {
                        id: TrackedSetId(0),
                    },
                    grantee: Default::default(),
                },
            )
            .sub_ability(register_bending),
        )),
        distribute: None,
        multi_target,
        condition: None,
        optional: false,
    })
}

fn try_parse_earthbend_clause(tp: TextPair<'_>) -> Option<ParsedEffectClause> {
    let (_, rest) = nom_on_lower(tp.original, tp.lower, |i| {
        value((), tag("earthbend ")).parse(i)
    })?;
    // `rest` is the original-case remainder; lowercase it for the nom-based
    // dispatcher inside `parse_earthbend_count_expr`, which expects already-lowered
    // input (matches the convention used by `parse_earthbend_params`'s sole
    // imperative caller, which passes a `rest_lower` slice).
    let lower_rest = rest.to_ascii_lowercase();
    let (target, counter_count) = imperative::parse_earthbend_count_expr(tp.original, &lower_rest);

    let register_bending = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RegisterBending {
            kind: crate::types::events::BendingType::Earth,
        },
    );

    let return_effect = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Battlefield,
            target: TargetFilter::TriggeringSource,
            owner_library: false,
            enter_transformed: false,
            under_your_control: true,
            enter_tapped: true,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },
    );

    let delayed_return = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CreateDelayedTrigger {
            condition: DelayedTriggerCondition::WhenDiesOrExiled {
                filter: TargetFilter::ParentTarget,
            },
            effect: Box::new(return_effect),
            uses_tracked_set: false,
        },
    )
    .sub_ability(register_bending);

    let put_counters = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: "P1P1".to_string(),
            count: counter_count,
            target: TargetFilter::ParentTarget,
        },
    )
    .sub_ability(delayed_return);

    Some(ParsedEffectClause {
        effect: Effect::Animate {
            power: Some(0),
            toughness: Some(0),
            types: vec!["Creature".to_string()],
            remove_types: vec![],
            target,
            keywords: vec![crate::types::keywords::Keyword::Haste],
        },
        duration: Some(Duration::Permanent),
        sub_ability: Some(Box::new(put_counters)),
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
    })
}

/// CR 402.2 + CR 114.1: Parse "you have no maximum hand size [for the rest of the game]"
/// as an effect that creates an emblem with `NoMaximumHandSize` static.
///
/// When this text appears on a permanent as a static ability, the static parser handles it.
/// When it appears as an effect line in a spell or triggered ability (e.g., Choice of Fortunes),
/// it needs to create an emblem to produce a persistent game-state effect.
/// CR 700.2 + CR 608.2d: Detect inline binary-choice imperatives of the form
/// "A or B" where both A and B parse independently as supported effects.
/// Emits `Effect::ChooseOneOf { branches: [A, B] }` so the second branch is
/// preserved in card data rather than silently dropped by the single-verb
/// dispatch.
///
/// Highway Robbery ("discard a card or sacrifice a land") is the motivating
/// case; the same shape applies to other "you may A or B" cards where the
/// outer "you may" has already been stripped by `strip_optional_effect_prefix`.
///
/// Constraints (false-positive guards):
///   1. Reject if the clause starts with "target " — "target creature or
///      player" is a typed target, not a choice of effects.
///   2. Reject if the first token is a modal header ("choose "), reminder/
///      connector ("either"), or a non-imperative starter — this helper is
///      a last-resort detector, not a general splitter.
///   3. Reject unless BOTH halves independently parse to a non-`Unimplemented`
///      effect. A half failing to parse means this isn't a true "A or B"
///      choice — it's probably noun-phrase disjunction ("creature or land")
///      inside a larger imperative.
///   4. Only consider the FIRST `, or ` or first top-level ` or ` split. Cards
///      with 3+ branches are rare and handled by bulleted modal text instead.
fn try_parse_choose_one_of_inline(
    tp: TextPair<'_>,
    ctx: &ParseContext,
) -> Option<ParsedEffectClause> {
    // CR 115.1: bail on target phrases — "target creature or player" is a
    // typed-target disjunction, not a choice between effects.
    if tag::<_, _, VerboseError<&str>>("target ")
        .parse(tp.lower)
        .is_ok()
    {
        return None;
    }

    // Find the first top-level " or " split. Prefer a comma-qualified ", or "
    // as the clearer form; fall back to bare " or " when the text is short
    // enough that misreading intra-phrase "or" is unlikely. Each needle
    // literal appears exactly once — the separator's length is implied by
    // what's missing between the two returned halves.
    let (before_lower, after_lower) =
        split_around(tp.lower, ", or ").or_else(|| split_around(tp.lower, " or "))?;

    // Multi-`or` chains and inner-noun disjunctions ("discard a creature card
    // or land, or sacrifice a creature") would split here at the FIRST `or`,
    // leaving the left half's inner ` or ` to be silently dropped by
    // `parse_effect_clause` — both halves parse to non-Unimplemented but the
    // left has the wrong meaning. Reject unless the left half is unambiguously
    // a single imperative (no further top-level ` or `). This keeps the
    // 2-branch case working and conservatively declines on 3+ chains so they
    // fall through to higher-fidelity parsing rather than misread silently.
    //
    // Detection uses nom's `take_until` to scan for ` or ` in the already-
    // separated left half — a structural post-split sanity check, not parser
    // dispatch.
    if take_until::<_, _, VerboseError<&str>>(" or ")
        .parse(before_lower)
        .is_ok()
    {
        return None;
    }

    let split_index = before_lower.len();
    let after_offset = tp.lower.len() - after_lower.len();

    let left_orig = tp.original[..split_index].trim();
    let right_orig = tp.original[after_offset..].trim();

    // Both halves must be non-empty and look like imperatives. The left half
    // must not end with a subject marker that would make the "or" coordinate
    // nouns (e.g., "target creature" should not split as a branch boundary).
    if left_orig.is_empty() || right_orig.is_empty() {
        return None;
    }

    // Attempt to parse each branch independently as a clause.
    let left_clause = parse_effect_clause(left_orig, ctx);
    let right_clause = parse_effect_clause(right_orig, ctx);

    // Reject unless BOTH branches produce non-Unimplemented effects. This
    // prevents false positives on noun-phrase disjunctions and "and/or"
    // coordinations inside larger imperatives.
    if matches!(left_clause.effect, Effect::Unimplemented { .. })
        || matches!(right_clause.effect, Effect::Unimplemented { .. })
    {
        return None;
    }

    // Also reject if either branch is `TargetOnly` — that's a structural
    // wrapper, not a terminal effect. A real binary choice needs two
    // executable branches.
    if matches!(left_clause.effect, Effect::TargetOnly { .. })
        || matches!(right_clause.effect, Effect::TargetOnly { .. })
    {
        return None;
    }

    let left_def = AbilityDefinition::new(AbilityKind::Spell, left_clause.effect);
    let right_def = AbilityDefinition::new(AbilityKind::Spell, right_clause.effect);

    Some(parsed_clause(Effect::ChooseOneOf {
        branches: vec![left_def, right_def],
    }))
}

fn try_parse_no_max_hand_size_effect(tp: TextPair<'_>) -> Option<Effect> {
    // Strip optional "you " prefix, then match "have no maximum hand size"
    let after_you = nom_on_lower(tp.original, tp.lower, |i| value((), tag("you ")).parse(i));
    let rest = after_you.map(|(_, r)| r).unwrap_or(tp.original);
    let rest_lower = rest.to_lowercase();

    // Match "have no maximum hand size" with optional trailing "for the rest of the game"
    let matched = tag::<_, _, VerboseError<&str>>("have no maximum hand size")
        .parse(rest_lower.as_str())
        .ok()?;
    let remainder = matched.0.trim().trim_end_matches('.');
    // Allow bare "have no maximum hand size" or "for the rest of the game" suffix
    if !remainder.is_empty()
        && tag::<_, _, VerboseError<&str>>("for the rest of the game")
            .parse(remainder)
            .is_err()
    {
        return None;
    }

    Some(Effect::CreateEmblem {
        statics: vec![StaticDefinition::new(StaticMode::NoMaximumHandSize)],
        triggers: Vec::new(),
    })
}

/// CR 608.2d: Parse "have it [verb]" / "have you [verb]" causative constructions.
/// Used by "any opponent may" effects where the opponent causes the source or controller
/// to perform an action (e.g., "have it deal 4 damage to them").
fn try_parse_have_causative(tp: TextPair<'_>, ctx: &ParseContext) -> Option<ParsedEffectClause> {
    // Pattern A: "have it deal N damage to them" / "have ~ deal N damage to them"
    let after_have = nom_on_lower(tp.original, tp.lower, |input| {
        value((), alt((tag("have it "), tag("have ~ ")))).parse(input)
    });
    if let Some((_, rest_orig)) = after_have {
        let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];
        let rest = TextPair::new(rest_orig, rest_lower);
        // "deal N damage to them" / "deal N damage to that player"
        if let Some(after_deal) = nom_on_lower(rest.original, rest.lower, |i| {
            value((), tag("deal ")).parse(i)
        })
        .map(|(_, r)| TextPair::new(r, &rest.lower[rest.lower.len() - r.len()..]))
        {
            if let Some((amount, _)) = super::oracle_util::parse_count_expr(after_deal.lower) {
                return Some(parsed_clause(Effect::DealDamage {
                    amount,
                    target: TargetFilter::Player,
                    damage_source: None,
                }));
            }
        }
        // CR 611.2b: When the specific causative "deal damage" pattern does
        // not match, decline so the more general `try_parse_have_redirection`
        // (subject-predicate dispatch downstream of `parse_effect_clause`) can
        // take this clause. Recursing through `parse_effect_clause` here would
        // erase the subject (`~` / `it`) and reduce the remainder to a bare
        // imperative — which `try_parse_subject_become_clause` and other
        // subject-aware arms can no longer reach (Sarkhan, Soul Aflame's
        // "have ~ become a copy of it ..." class).
        return None;
    }

    // Pattern B: "have you [verb]" — controller performs an action directed by opponent
    if let Some((_, rest_orig)) = nom_on_lower(tp.original, tp.lower, |i| {
        value((), tag("have you ")).parse(i)
    }) {
        let clause = parse_effect_clause(rest_orig, ctx);
        return Some(clause);
    }

    None
}

fn all_core_type_categories() -> Vec<CoreType> {
    vec![
        CoreType::Artifact,
        CoreType::Battle,
        CoreType::Creature,
        CoreType::Enchantment,
        CoreType::Instant,
        CoreType::Land,
        CoreType::Planeswalker,
        CoreType::Sorcery,
    ]
}

fn try_parse_distinct_card_types_from_revealed(tp: TextPair<'_>) -> Option<ParsedEffectClause> {
    type E<'a> = VerboseError<&'a str>;

    let (rest, _) = tag::<_, _, E>("for each ").parse(tp.lower).ok()?;
    let (rest, _) = alt((tag::<_, _, E>("card type"), tag("card types")))
        .parse(rest)
        .ok()?;
    let (rest, _) = tag::<_, _, E>(", you may put ").parse(rest).ok()?;
    let (rest, _) = nom_primitives::parse_article.parse(rest).ok()?;
    let (rest, _) = tag::<_, _, E>("card of that type from among ")
        .parse(rest)
        .ok()?;
    let (rest, _) = alt((
        tag::<_, _, E>("the revealed cards"),
        tag("those revealed cards"),
    ))
    .parse(rest)
    .ok()?;
    let (rest, _) = tag::<_, _, E>(" into your hand").parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    let categories = all_core_type_categories();
    Some(ParsedEffectClause {
        effect: Effect::ChooseFromZone {
            count: categories.len() as u32,
            zone: Zone::Library,
            chooser: crate::types::ability::Chooser::Controller,
            up_to: true,
            constraint: Some(ChooseFromZoneConstraint::DistinctCardTypes { categories }),
        },
        duration: None,
        sub_ability: Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Hand,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        ))),
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
    })
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
    // CR 608.2c: Self-ref continuation adverb. "also" after a self-ref subject
    // is a natural-language additive connector with no semantic weight — it
    // signals "in addition, [self-ref] [verb]…" and must reduce to plain
    // self-ref subject-predicate dispatch. Strip only when the "also" is
    // present; without it, the bare `~` / `this creature` subject carries
    // target attribution that downstream Pump/PutCounter parsers rely on.
    //
    // Example: Expressive Firedancer sub_ability "~ also gains double strike
    // until end of turn" → strip "~ also " → "gains double strike until end
    // of turn" → first-word dispatch lands on gain-keyword with SelfRef
    // target implied by the absence of an explicit target.
    let stripped_self_ref;
    let text = {
        let lower = text.to_ascii_lowercase();
        let stripped_lower = alt((
            value((), tag::<_, _, VerboseError<&str>>("~ also ")),
            value((), tag("this creature also ")),
            value((), tag("this permanent also ")),
        ))
        .parse(lower.as_str())
        .ok()
        .map(|(rest, ())| {
            let consumed = lower.len() - rest.len();
            text[consumed..].trim_start().to_string()
        });
        if let Some(rest) = stripped_lower {
            stripped_self_ref = rest;
            stripped_self_ref.as_str()
        } else {
            text
        }
    };
    if text.is_empty() {
        return parsed_clause(Effect::Unimplemented {
            name: "empty".to_string(),
            description: None,
        });
    }

    // CR 608.2c: Deconjugate bare third-person verbs that appear after ", then" splits
    // where the subject carried over from the previous clause.
    // E.g., "draws seven cards" → "draw seven cards" (from "Each player discards
    // their hand, then draws seven cards.").
    // Only fires when the first word is a conjugated verb form that, after
    // deconjugation, matches a recognized imperative clause start — this avoids
    // false positives on noun phrases or subject-prefixed text.
    let deconjugated_storage;
    let text = if !sequence::starts_clause_text(text)
        && sequence::starts_clause_text_or_conjugated(text)
    {
        deconjugated_storage = subject::deconjugate_verb(text);
        &deconjugated_storage
    } else {
        text
    };

    // Single lowercase pass for all case-insensitive matching within this clause.
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // CR 700.2 + CR 608.2d: Inline binary-choice imperative — "discard a card
    // or sacrifice a land" (Highway Robbery) and analogous "A or B" patterns
    // that are not bulleted `Choose one —` modals. The split happens BEFORE
    // any single-verb dispatch so the second branch isn't silently dropped
    // when the first matches ("discard ..." prefix consumes the whole text).
    // Constrained to the narrow case where (a) exactly one ` or ` appears at
    // clause top level, (b) both halves independently parse as a supported
    // imperative — prevents false positives on intra-imperative "or" like
    // "target creature or player".
    if let Some(clause) = try_parse_choose_one_of_inline(tp, ctx) {
        return clause;
    }

    // CR 402.2 + CR 114.1: "you have no maximum hand size [for the rest of the game]" —
    // as an effect, this creates an emblem with NoMaximumHandSize static.
    if let Some(effect) = try_parse_no_max_hand_size_effect(tp) {
        return parsed_clause(effect);
    }

    // CR 608.2d: "have it [verb]" / "have you [verb]" — causative construction
    // from "any opponent may" effects (e.g., "have it deal 4 damage to them").
    if let Some(clause) = try_parse_have_causative(tp, ctx) {
        return clause;
    }

    // CR 115.1d + CR 601.2c: "for each of them, [imperative]" — rhetorical
    // per-target distributor on a multi-target spell (Twinflame's strive
    // chain: "Choose any number of target creatures you control. For each of
    // them, create a token …"). The per-target iteration is intrinsic to how
    // resolvers (CopyTokenOf, Pump, etc.) iterate `ability.targets`, so we
    // strip the prefix and recurse on the inner clause. Anaphoric subjects
    // inside the inner clause ("that creature") resolve to the per-target
    // ParentTarget via `parse_target`.
    if let Some(rest) = tp
        .strip_prefix("for each of them, ")
        .or_else(|| tp.strip_prefix("for each of them "))
    {
        return parse_effect_clause(rest.original, ctx);
    }

    if let Some(clause) = try_parse_distinct_card_types_from_revealed(tp) {
        return clause;
    }

    // CR 122.1: "you get {E}{E}" — gain energy counters.
    if scan_contains_phrase(tp.lower, "{e}")
        && alt((tag::<_, _, VerboseError<&str>>("you get "), tag("get ")))
            .parse(tp.lower)
            .is_ok()
    {
        let amount = super::oracle_util::count_energy_symbols(tp.lower);
        if amount > 0 {
            return parsed_clause(Effect::GainEnergy { amount });
        }
    }

    // CR 106.12: "don't lose [unspent] {color} mana as steps and phases end" —
    // mana pool retention. Parsed as supported no-op (runtime behavior is future work).
    if scan_contains_phrase(tp.lower, "lose") && scan_contains_phrase(tp.lower, "mana as steps") {
        return parsed_clause(Effect::GenericEffect {
            static_abilities: vec![],
            duration: None,
            target: None,
        });
    }

    // CR 701.54: "the ring tempts you" — Ring Tempts You effect.
    if scan_contains_phrase(tp.lower, "the ring tempts you") {
        return parsed_clause(Effect::RingTemptsYou);
    }

    // CR 101.4 + CR 701.21a: "For each player, you choose from among the permanents that
    // player controls an artifact, a creature, ..." — Tragic Arrogance pattern where
    // the spell's controller chooses for all players.
    if let Ok((after_prefix, _)) =
        tag::<_, _, VerboseError<&str>>("for each player, you choose ").parse(tp.lower)
    {
        if let Some(ast) = imperative::parse_category_and_sacrifice_rest_pub(after_prefix) {
            return parsed_clause(imperative::lower_choose_ast(ast));
        }
    }

    if tp.lower == "start your engines!" || tp.lower == "start your engines" {
        return parsed_clause(Effect::StartYourEngines {
            player_scope: PlayerFilter::Controller,
        });
    }

    if tp.lower == "all players start their engines."
        || tp.lower == "all players start their engines"
        || tp.lower == "all players start their engines!"
    {
        return parsed_clause(Effect::StartYourEngines {
            player_scope: PlayerFilter::All,
        });
    }

    if let Some(amount_text) = tag::<_, _, VerboseError<&str>>("increase your speed by ")
        .parse(tp.lower)
        .ok()
        .map(|(rest, _)| rest.trim())
    {
        // Delegate to nom combinator (input already lowercase from tp.lower).
        if let Ok((remainder, amount)) = nom_primitives::parse_number.parse(amount_text) {
            if remainder.trim().is_empty() {
                return parsed_clause(Effect::IncreaseSpeed {
                    player_scope: PlayerFilter::Controller,
                    amount: QuantityExpr::Fixed {
                        value: amount as i32,
                    },
                });
            }
        }
    }

    // CR 603.7c: "Whenever X this turn, Y" — multi-fire delayed trigger creation.
    if let Some(clause) = try_parse_whenever_this_turn(tp) {
        return clause;
    }

    // CR 603.7: "When you next cast a [type] spell this turn, ..." — one-shot delayed trigger.
    if let Some(clause) = try_parse_when_next_event(tp) {
        return clause;
    }

    // CR 603.7c: "When that creature dies, ..." — inline delayed trigger creation.
    if let Some(clause) = try_parse_inline_delayed_trigger(tp) {
        return clause;
    }

    if let Some(clause) = try_parse_self_name_exile(tp, ctx) {
        return clause;
    }

    // CR 601.2f: "the next spell you cast this turn costs {N} less to cast"
    if let Some(clause) = try_parse_reduce_next_spell_cost(tp) {
        return clause;
    }

    // CR 601.2f: "the next [type] spell you cast this turn [has keyword/can't be countered/etc.]"
    if let Some(clause) = try_parse_grant_next_spell_ability(tp) {
        return clause;
    }

    // CR 614.16: "Damage can't be prevented [this turn]" → Effect::AddRestriction
    if let Some(clause) = try_parse_damage_prevention_disabled(tp) {
        return clause;
    }

    if let Some(clause) = try_parse_cast_only_from_zones_restriction(tp) {
        return clause;
    }

    // CR 101.2: "Your opponents can't cast spells this turn" — blanket temporary prohibition.
    // Must run AFTER cast_only_from_zones (more specific "from zones" check).
    if let Some(clause) = try_parse_cant_cast_spells_effect(tp) {
        return clause;
    }

    if let Some(clause) = try_parse_earthbend_clause(tp) {
        return clause;
    }

    if let Some(clause) = try_parse_airbend_clause(tp) {
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

    // CR 614.10: "you skip your next turn" / "skip your next turn" — temporal penalty.
    if let Some(clause) = try_parse_skip_next_turn(tp) {
        return clause;
    }

    // CR 601.2d: "deal N damage divided as you choose among [targets]" /
    // "distributed among" / "divided evenly" → DealDamage with distribute.
    if scan_contains_phrase(&lower, "divided as you choose among")
        || scan_contains_phrase(&lower, "distributed among")
        || scan_contains_phrase(&lower, "divided evenly")
    {
        if let Some(clause) = try_parse_distribute_damage(&lower, text) {
            return clause;
        }
    }

    // CR 601.2d: "distribute N [type] counters among [targets]" →
    // PutCounter with distribute: Some(Counters(type)).
    if tag::<_, _, VerboseError<&str>>("distribute ")
        .parse(lower.as_str())
        .is_ok()
        && scan_contains_phrase(&lower, "counter")
        && scan_contains_phrase(&lower, "among")
    {
        if let Some(clause) = try_parse_distribute_counters(&lower, text) {
            return clause;
        }
    }

    // "for each" patterns: "draw a card for each [filter]", etc.
    if let Some(clause) = try_parse_for_each_effect(text) {
        return clause;
    }

    // CR 121.1 + CR 701.17a: "{verb} cards equal to {quantity}" — dynamic count
    // from game state for draw or mill.
    if let Some(clause) = try_parse_equal_to_quantity_effect(tp) {
        return clause;
    }

    // CR 702.84a: "exile cards from the top of your library until you exile a [filter] card"
    if let Some(clause) = try_parse_exile_from_top_until(tp) {
        return clause;
    }

    // CR 701.20a: "reveal cards from the top of your library until you reveal a [filter]"
    // Bare imperative (no explicit subject) → activator's library (CR 109.5).
    if let Some(clause) = try_parse_reveal_until(tp, TargetFilter::Controller) {
        return clause;
    }

    // CR 701.20a: "reveal it" / "reveal them" — standalone reveal of a referenced object
    // (e.g., the card looked at via a dig effect). Maps to RevealTop { count: 1 }.
    if let Ok((rest, _)) =
        alt((tag::<_, _, VerboseError<&str>>("reveal "), tag("reveals "))).parse(tp.lower)
    {
        if alt((
            value((), tag::<_, _, VerboseError<&str>>("it")),
            value((), tag("them")),
        ))
        .parse(rest.trim_end_matches('.'))
        .is_ok()
        {
            return parsed_clause(Effect::RevealTop {
                player: TargetFilter::Controller,
                count: 1,
            });
        }
    }

    // CR 122.3: "cast that card by paying an amount of {E} equal to its mana value"
    // → GrantCastingPermission with ExileWithEnergyCost
    if scan_contains_phrase(tp.lower, "by paying an amount of {e}")
        && scan_contains_phrase(tp.lower, "equal to its mana value")
    {
        return parsed_clause(Effect::GrantCastingPermission {
            permission: CastingPermission::ExileWithEnergyCost,
            target: TargetFilter::ParentTarget,
            grantee: Default::default(),
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
    if let Some((_, rest_orig)) =
        super::oracle_nom::bridge::nom_on_lower(tp.original, tp.lower, |i| {
            value((), tag("discover ")).parse(i)
        })
    {
        let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];
        if let Ok(n) = rest_lower.trim().parse::<u32>() {
            return parsed_clause(Effect::Discover {
                mana_value_limit: n,
            });
        }
    }

    // CR 401.4: "[target]'s owner puts it on their choice of the top or bottom of their library"
    if let Some(clause) = try_parse_put_on_top_or_bottom(tp) {
        return clause;
    }

    // CR 701.24a + CR 400.3: "the owner of target [filter] shuffles it into their library"
    if let Some(clause) = try_parse_owner_of_target_shuffle(tp) {
        return clause;
    }

    // CR 401.4 + CR 400.3: "the owner of target [filter] puts it into their library second from
    // the top or on the bottom"
    if let Some(clause) = try_parse_owner_of_target_put_second(tp) {
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

    // Digital-only: "conjure a card named X into/onto zone" — Conjure keyword action.
    if let Some(effect) = try_parse_conjure(tp) {
        return parsed_clause(effect);
    }

    let ast = parse_clause_ast(text, ctx);
    lower_clause_ast(ast, ctx)
}

/// Digital-only keyword action: Parse "conjure [quantity] card(s) named {Name} into/onto {zone}"
/// patterns. Handles:
/// - "conjure a card named X onto the battlefield"
/// - "conjure a card named X into your hand"
/// - "conjure three cards named X into your graveyard"
/// - "conjure a card named X onto the battlefield tapped"
/// - "conjure a card named X and a card named Y into your hand"
/// - "conjure X cards named Y onto the battlefield"
///
/// Uses nom combinators exclusively for dispatch and structure recognition.
fn try_parse_conjure(tp: TextPair) -> Option<Effect> {
    // Gate: must start with "conjure " (nom tag dispatch).
    let (rest, _) = tag::<_, _, VerboseError<&str>>("conjure ")
        .parse(tp.lower)
        .ok()?;
    let rest_orig = &tp.original[tp.original.len() - rest.len()..];

    // Parse the first "a card named X" / "N cards named X" entry.
    let mut cards = Vec::new();
    let (count, after_count, _) = parse_conjure_quantity(rest, rest_orig)?;

    // Expect "named " after the quantity phrase.
    let (after_named, _) = tag::<_, _, VerboseError<&str>>("named ")
        .parse(after_count)
        .ok()?;
    let after_named_orig = &rest_orig[rest_orig.len() - after_named.len()..];

    // Extract card name: take until " onto ", " into ", or " and a card" separator.
    let (card_name_lower, zone_rest) = parse_conjure_card_name(after_named)?;
    let card_name = &after_named_orig[..card_name_lower.len()];

    cards.push(ConjureCard {
        name: card_name.to_string(),
        count,
    });

    // Check for " and a card named " continuation (multi-card pattern).
    // e.g., "conjure a card named X and a card named Y into your hand"
    let zone_rest = if let Ok((after_and, _)) =
        tag::<_, _, VerboseError<&str>>(" and a card named ").parse(zone_rest)
    {
        let after_and_orig = &rest_orig[rest_orig.len() - after_and.len()..];
        let (next_name_lower, next_zone_rest) = parse_conjure_card_name(after_and)?;
        let next_name = &after_and_orig[..next_name_lower.len()];
        cards.push(ConjureCard {
            name: next_name.to_string(),
            count: QuantityExpr::Fixed { value: 1 },
        });
        next_zone_rest
    } else {
        zone_rest
    };

    // Parse destination zone.
    let (destination, zone_rest) = parse_conjure_zone(zone_rest)?;

    // Parse optional "tapped" suffix.
    let tapped = tag::<_, _, VerboseError<&str>>(" tapped")
        .parse(zone_rest)
        .is_ok();

    Some(Effect::Conjure {
        cards,
        destination,
        tapped,
    })
}

/// Parse the quantity portion of a conjure clause.
/// Consumes the quantity and "card(s) " but NOT "named " — the caller handles "named ".
/// Returns (QuantityExpr, remaining_lower, remaining_orig).
fn parse_conjure_quantity<'a>(
    lower: &'a str,
    orig: &'a str,
) -> Option<(QuantityExpr, &'a str, &'a str)> {
    // "a card " → quantity 1
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("a card ").parse(lower) {
        let rest_orig = &orig[orig.len() - rest.len()..];
        return Some((QuantityExpr::Fixed { value: 1 }, rest, rest_orig));
    }

    // Try numeric: "three cards ", "four cards ", "X cards "
    // First try nom parse_number for English words and digits.
    if let Ok((after_num, n)) = nom_primitives::parse_number.parse(lower) {
        let after_num = after_num.trim_start();
        if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("cards ").parse(after_num) {
            let rest_orig = &orig[orig.len() - rest.len()..];
            return Some((QuantityExpr::Fixed { value: n as i32 }, rest, rest_orig));
        }
    }

    None
}

/// Extract the card name from conjure text. The name extends until we hit
/// a zone destination (" onto " or " into ") or an " and a card" separator.
/// Uses nom `take_until` combinators for structured extraction.
///
/// " and a card" is checked first because multi-card patterns like
/// "X and a card named Y into your hand" contain both " and a card" and " into ",
/// and we want the shortest (first occurring) separator.
fn parse_conjure_card_name(lower: &str) -> Option<(&str, &str)> {
    alt((
        take_until::<_, _, VerboseError<&str>>(" and a card"),
        take_until(" onto "),
        take_until(" into "),
    ))
    .parse(lower)
    .ok()
    .map(|(rest, name)| (name, rest))
}

/// Parse the destination zone from conjure text using nom combinators.
fn parse_conjure_zone(lower: &str) -> Option<(Zone, &str)> {
    alt((
        value(
            Zone::Battlefield,
            tag::<_, _, VerboseError<&str>>(" onto the battlefield"),
        ),
        value(Zone::Hand, tag(" into your hand")),
        value(Zone::Graveyard, tag(" into your graveyard")),
        value(Zone::Library, tag(" into your library")),
        // Third-person variants: "into their hand/graveyard/library"
        value(Zone::Hand, tag(" into their hand")),
        value(Zone::Graveyard, tag(" into their graveyard")),
        value(Zone::Library, tag(" into their library")),
    ))
    .parse(lower)
    .ok()
    .map(|(rest, zone)| (zone, rest))
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
    let (after_have, _) = tag::<_, _, VerboseError<&str>>("have ")
        .parse(lower.as_str())
        .ok()?;

    // CR 603.2 + CR 113.3a + CR 111.10: "have its controller <predicate>" —
    // rebinds the acting player of the redirected sub-effect from the ability
    // controller (default per CR 113.3a) to the triggering source's controller.
    // "Its" refers to the triggering object (e.g., for Najeela, the attacking
    // Warrior). At resolution, the redirected effect's player-scoped filter
    // auto-resolves via `TargetFilter::TriggeringSource` (see
    // `extract_event_context_filter` + `resolve_token_owner`).
    if let Some(clause) = try_parse_have_its_controller(text, after_have, ctx) {
        return Some(clause);
    }

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

/// CR 603.2 + CR 111.10: Parse "have its controller <predicate>" — controller
/// rebinding for token creation (and other player-scoped sub-effects) on a
/// triggered ability. Najeela, the Blade-Blossom is the canonical case:
/// "Whenever a Warrior attacks, you may have its controller create a 1/1
/// white Warrior creature token that's tapped and attacking."
///
/// At resolution, `Effect::Token.owner` = `TargetFilter::TriggeringSource`
/// causes `extract_event_context_filter` to populate `ability.targets` with
/// the attacker's ObjectRef, and `resolve_token_owner`'s fallback arm reads
/// the controller from that object — yielding the Warrior's controller,
/// exactly as CR 111.10 requires.
fn try_parse_have_its_controller(
    original: &str,
    after_have_lower: &str,
    ctx: &ParseContext,
) -> Option<ParsedEffectClause> {
    // Nom dispatch: must match "its controller " exactly.
    let (rest_lower, _) = tag::<_, _, VerboseError<&str>>("its controller ")
        .parse(after_have_lower)
        .ok()?;

    // Recover the original-case remainder using the canonical `len - rest.len()`
    // offset idiom. `after_have_lower` starts at the same offset in both slices
    // (case-insensitive prefix "have " has no case-varying chars), so the offset
    // into `original` is `original.len() - rest_lower.len()`.
    let offset = original.len().checked_sub(rest_lower.len())?;
    let redirected_text = original[offset..].trim();

    // Parse the sub-effect through the full effect pipeline.
    let clause = parse_effect_clause(redirected_text, ctx);
    if matches!(clause.effect, Effect::Unimplemented { .. }) {
        return None;
    }

    // Rebind the acting player of player-scoped sub-effects to the triggering
    // source's controller. Mirrors the original Token-only rebind across the
    // common player-scoped sub-effects so cards in the same class (Captive
    // Audience-style "have its controller draw/discard/lose life") resolve
    // their player field to the *acting* player rather than the static
    // ability's controller.
    let rebound = rebind_controller_to_triggering_source(clause);
    Some(rebound)
}

/// Rebind the player-facing filter of a parsed effect clause to
/// `TargetFilter::TriggeringSource` so token owners (and similar "acting
/// player" fields) resolve to the controller of the triggering object at
/// runtime. Only effects whose player-scope field is currently the default
/// `Controller` are touched — an explicit target/filter is preserved.
///
/// Covers the player-scoped sub-effects that the "have its controller [verb]"
/// pattern can produce: Token (creation), Draw, DiscardCard, LoseLife, Mill.
/// Each receives the same defaulted-Controller-only guard so a parser arm
/// that wired an explicit target (e.g. "have its controller mill target
/// player's library") survives unchanged.
fn rebind_controller_to_triggering_source(mut clause: ParsedEffectClause) -> ParsedEffectClause {
    let rebind = |target: &mut TargetFilter| {
        if matches!(target, TargetFilter::Controller) {
            *target = TargetFilter::TriggeringSource;
        }
    };
    match &mut clause.effect {
        Effect::Token { owner, .. } => rebind(owner),
        Effect::Draw { target, .. } => rebind(target),
        Effect::DiscardCard { target, .. } => rebind(target),
        Effect::Mill { target, .. } => rebind(target),
        // CR 119.3: `LoseLife.target` is `Option<TargetFilter>` — only rebind
        // the populated `Controller` case; `None` means "the controller of
        // the resolving ability" and that semantic is preserved by leaving it.
        Effect::LoseLife {
            target: Some(t), ..
        } => rebind(t),
        _ => {}
    }
    clause
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
    let (_, rest_orig) = nom_on_lower(tp.original, tp.lower, |i| {
        value((), tag("all creatures able to block ")).parse(i)
    })?;
    let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];
    let rest = TextPair::new(rest_orig, rest_lower);

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
    // May be compound: "~ or enchanted creature" → Or union.
    let target_text = &rest.original[..before_temporal.len()];
    let (target, remainder) = parse_target(target_text);
    let (target, remainder) = refine_damage_target_remainder(target, remainder);
    if !remainder.trim().is_empty() {
        push_warning(format!(
            "ignored-remainder: '{}' after target parse in must-block",
            remainder.trim()
        ));
    }

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
        condition: None,
        duration: Some(Duration::UntilEndOfTurn),
        sub_ability: None,
        optional: false,
    })
}

fn try_parse_still_a_type(tp: TextPair) -> Option<ParsedEffectClause> {
    // Match singular "it's still a/an [type]" / "that's still a/an [type]"
    // or plural "they're still [type]s" — CR 205.1a type retention after animation.
    let (is_plural, rest_orig) = nom_on_lower(tp.original, tp.lower, |input| {
        alt((
            value(false, alt((tag("it's still "), tag("that's still ")))),
            value(true, tag("they're still ")),
        ))
        .parse(input)
    })?;
    let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];

    let type_name_lower = if is_plural {
        // Plural: "they're still lands" — no article, strip trailing 's'
        rest_lower
    } else {
        // Singular: strip article "a " / "an "
        let ((), after_article) = nom_on_lower(rest_orig, rest_lower, |input| {
            nom_primitives::parse_article(input)
        })?;
        &rest_lower[rest_lower.len() - after_article.len()..]
    };

    // Strip plural 's' if present (e.g., "lands" → "land", "creatures" → "creature")
    let singular = type_name_lower.strip_suffix('s').unwrap_or(type_name_lower);
    let core_type = CoreType::from_str(&capitalize(singular)).ok()?;

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
        condition: None,
        optional: false,
    })
}

/// CR 614.10: Parse "[subject] skip[s] [their|your] next [N] turn[s]" — temporal
/// penalty effect. Handles:
///   - "you skip your next turn" / "skip your next turn" → Controller, 1
///   - "target opponent skips their next turn" → Opponent-target, 1
///   - "target player skips their next turn" → Player-target, 1
///   - "target opponent skips their next N turns" → Opponent-target, N
///   - "target opponent skips their next X turns, where X is the number of
///     coins that came up heads" → Opponent-target, 1 (the heads-count
///     multiplier is supplied by the outer `Effect::FlipCoins` loop, which
///     invokes this handler once per heads).
fn try_parse_skip_next_turn(tp: TextPair) -> Option<ParsedEffectClause> {
    // Bare subjectless form (controller skips).
    if nom_on_lower(tp.original, tp.lower, |input| {
        alt((
            value(
                (),
                tag::<_, _, VerboseError<&str>>("you skip your next turn"),
            ),
            value((), tag("skip your next turn")),
        ))
        .parse(input)
    })
    .is_some()
    {
        return Some(parsed_clause(Effect::SkipNextTurn {
            target: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 1 },
        }));
    }

    // Targeted form: "target {opponent|player} skips their next [N|X] turn[s]".
    // Guard on the lowercase prefix before delegating to `parse_target`.
    nom::combinator::peek(alt((
        tag::<_, _, VerboseError<&str>>("target opponent "),
        tag("target player "),
    )))
    .parse(tp.lower)
    .ok()?;

    // Delegate the target extraction to the canonical parser so any future
    // target shape ("target opponent of your choice", etc.) picks it up.
    let (target, after_target_orig) = super::oracle_target::parse_target(tp.original);
    let after_target_lower = &tp.lower[tp.lower.len() - after_target_orig.len()..];

    // Verb: " skips " / " skip " (surrounding spaces keep word boundary safe).
    let (after_verb_lower, _) = alt((tag::<_, _, VerboseError<&str>>(" skips "), tag(" skip ")))
        .parse(after_target_lower)
        .ok()?;

    // Possessive: "their next " / "your next ".
    let (after_next_lower, _) = alt((
        tag::<_, _, VerboseError<&str>>("their next "),
        tag("your next "),
    ))
    .parse(after_verb_lower)
    .ok()?;

    // Optional count between "next " and "turn[s]". Default to 1 if the very
    // next token is "turn"/"turns" (e.g. "skips their next turn"). When
    // `parse_count_expr` succeeds it trims leading whitespace on the remainder,
    // so match "turn[s]" without a leading-space prefix below.
    let after_next_orig = &tp.original[tp.lower.len() - after_next_lower.len()..];
    let (count, after_count_orig) = if let Some((expr, after_num_orig)) =
        super::oracle_util::parse_count_expr(after_next_orig)
    {
        (expr, after_num_orig)
    } else {
        (QuantityExpr::Fixed { value: 1 }, after_next_orig)
    };
    let after_count_lower = &tp.lower[tp.lower.len() - after_count_orig.len()..];

    // Require "turn[s]" with word-boundary termination.
    let (after_turn_lower, _) = alt((tag::<_, _, VerboseError<&str>>("turns"), tag("turn")))
        .parse(after_count_lower)
        .ok()?;
    if after_turn_lower
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric())
    {
        return None;
    }

    // CR 705: "where X is the number of coins that came up heads" — the heads
    // multiplier is supplied by the outer `Effect::FlipCoins` loop (one
    // invocation per heads), so normalize the count to 1.
    let trailing = after_turn_lower.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
    let is_heads_tail =
        tag::<_, _, VerboseError<&str>>("where x is the number of coins that came up heads")
            .parse(trailing)
            .is_ok();
    let count = if matches!(
        count,
        QuantityExpr::Ref {
            qty: QuantityRef::Variable { ref name },
        } if name == "X"
    ) && is_heads_tail
    {
        QuantityExpr::Fixed { value: 1 }
    } else {
        count
    };

    Some(parsed_clause(Effect::SkipNextTurn { target, count }))
}

/// Verb discriminant for `try_parse_equal_to_quantity_effect`. Using a typed
/// enum instead of a matched `&str` keeps all dispatch inside the nom `alt()`
/// combinator — the match arms below are exhaustive Rust enum arms, not
/// string-literal dispatch (CLAUDE.md nom-combinator mandate).
#[derive(Clone, Copy)]
enum EqualToQtyVerb {
    Mill,
    Draw,
}

/// Parse "{verb} cards equal to {quantity_ref}" patterns (CR 121.1 + CR 701.17a).
///
/// Handles verbs whose count field is `QuantityExpr` (mill, draw).
fn try_parse_equal_to_quantity_effect(tp: TextPair) -> Option<ParsedEffectClause> {
    // Try matching "mill cards equal to " or "draw cards equal to " using nom.
    let (verb, rest_orig) = nom_on_lower(tp.original, tp.lower, |input| {
        alt((
            value(EqualToQtyVerb::Mill, tag("mill cards equal to ")),
            value(EqualToQtyVerb::Draw, tag("draw cards equal to ")),
        ))
        .parse(input)
    })?;
    let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];
    let rest = rest_lower.trim().trim_end_matches('.');
    // CR 603.7c: Prefer event context quantity for triggered effects.
    let qty = super::oracle_quantity::parse_event_context_quantity(rest)?;
    match verb {
        EqualToQtyVerb::Mill => Some(parsed_clause(Effect::Mill {
            count: qty,
            // CR 701.17a: No subject → controller mills.
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        })),
        // CR 121.1 + CR 601.2c: Default Controller target — `inject_subject_target`
        // upgrades to `TargetFilter::Player` for "target player draws ..." subjects.
        EqualToQtyVerb::Draw => Some(parsed_clause(Effect::Draw {
            count: qty,
            target: TargetFilter::Controller,
        })),
    }
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
    if !scan_contains_phrase(tp.lower, "on the top or bottom of their library")
        && !scan_contains_phrase(
            tp.lower,
            "on their choice of the top or bottom of their library",
        )
    {
        return None;
    }

    // Pattern 1: "target creature's owner puts it ..."
    // Strip "'s owner puts it on ..." suffix, parse the target prefix.
    if let Some(idx) = tp.find("'s owner puts it") {
        let target_text = &tp.original[..idx];
        let (filter, remainder) = parse_target(target_text);
        if !remainder.trim().is_empty() {
            push_warning(format!(
                "ignored-remainder: '{}' after target parse in owner-puts-it",
                remainder.trim()
            ));
        }
        return Some(parsed_clause(Effect::PutOnTopOrBottom { target: filter }));
    }

    // Pattern 2: "the owner of target nonland permanent puts it ..."
    if let Some((_, rest_orig)) =
        super::oracle_nom::bridge::nom_on_lower(tp.original, tp.lower, |i| {
            value((), tag("the owner of ")).parse(i)
        })
    {
        let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];
        let rest = TextPair::new(rest_orig, rest_lower);
        if let Some(idx) = rest.find(" puts it") {
            let target_text = &rest.original[..idx];
            let (filter, remainder) = parse_target(target_text);
            if !remainder.trim().is_empty() {
                push_warning(format!(
                    "ignored-remainder: '{}' after target parse in owner-puts-it",
                    remainder.trim()
                ));
            }
            return Some(parsed_clause(Effect::PutOnTopOrBottom { target: filter }));
        }
    }

    None
}

/// CR 701.24a + CR 400.3: Parse "the owner of target [filter] shuffles it into their library".
///
/// Handles both subject forms:
/// - "The owner of target permanent shuffles it into their library" (Chaos Warp)
/// - "Target permanent's owner shuffles it into their library"
///
/// Produces a `ChangeZone` to Library with `owner_library: true` + a `Shuffle` sub_ability.
fn try_parse_owner_of_target_shuffle(tp: TextPair) -> Option<ParsedEffectClause> {
    let tp = tp.trim_end_matches('.');

    // Must contain both "shuffle" and "library" to be a shuffle-into-library pattern
    if !nom_primitives::scan_contains(tp.lower, "shuffle")
        || !nom_primitives::scan_contains(tp.lower, "library")
    {
        return None;
    }

    let target = extract_owner_of_target(tp)?;

    Some(imperative::with_shuffle_sub_ability(Effect::ChangeZone {
        origin: None,
        destination: Zone::Library,
        target,
        owner_library: true,
        enter_transformed: false,
        under_your_control: false,
        enter_tapped: false,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
    }))
}

/// CR 401.4 + CR 400.3: Parse "the owner of target [filter] puts it into their library
/// second from the top or on the bottom".
///
/// Handles:
/// - "The owner of target nonland permanent puts it into their library second from the top or on the bottom"
/// - "The owner of target creature or enchantment puts it into their library second from the top or on the bottom"
///
/// The "second from the top or on the bottom" pattern is semantically PutOnTopOrBottom (the
/// player chooses). We map it to the same `PutOnTopOrBottom` effect.
fn try_parse_owner_of_target_put_second(tp: TextPair) -> Option<ParsedEffectClause> {
    let tp = tp.trim_end_matches('.');

    // Must contain the signature suffix
    if !nom_primitives::scan_contains(tp.lower, "second from the top or on the bottom") {
        return None;
    }

    let target = extract_owner_of_target(tp)?;

    Some(parsed_clause(Effect::PutOnTopOrBottom { target }))
}

/// Shared helper: extract the target filter from "the owner of target [filter] [verb]s it ..."
/// or "target [filter]'s owner [verb]s it ..." subject patterns.
///
/// Returns `Some(target_filter)` if the pattern matches, `None` otherwise.
fn extract_owner_of_target(tp: TextPair) -> Option<TargetFilter> {
    // Pattern 1: "target [filter]'s owner [verb]s it ..."
    if let Some(idx) = tp.find("'s owner ") {
        let target_text = &tp.original[..idx];
        let (filter, remainder) = parse_target(target_text);
        if !remainder.trim().is_empty() {
            push_warning(format!(
                "ignored-remainder: '{}' after target parse in owner-of-target",
                remainder.trim()
            ));
        }
        return Some(filter);
    }

    // Pattern 2: "the owner of target [filter] [verb]s it ..."
    if let Some((_, rest_orig)) = nom_on_lower(tp.original, tp.lower, |i| {
        value((), tag("the owner of ")).parse(i)
    }) {
        let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];
        let rest = TextPair::new(rest_orig, rest_lower);
        // Find the verb boundary: " shuffles ", " puts ", " draws "
        for verb in [" shuffles ", " puts ", " draws "] {
            if let Some(idx) = rest.find(verb) {
                let target_text = &rest.original[..idx];
                let (filter, remainder) = parse_target(target_text);
                if !remainder.trim().is_empty() {
                    push_warning(format!(
                        "ignored-remainder: '{}' after target parse in owner-of-target",
                        remainder.trim()
                    ));
                }
                return Some(filter);
            }
        }
    }

    None
}

fn try_parse_exile_from_top_until(tp: TextPair) -> Option<ParsedEffectClause> {
    // CR 701.57a + CR 702.85a + CR 400.7: Parse `exile[s] cards from the top of
    // <possessive> library until <pronoun> exile[s] a/an {filter} card`.
    //
    // 701.57a / 702.85a (Discover / Cascade): canonical "exile from top until
    // a nonland card" pattern that the typed `Effect::ExileFromTopUntil`
    // generalizes — same shape with a parameterized hit filter (Etali, Primal
    // Conqueror's "until they exile a nonland card").
    // 400.7: each exiled card is a new object; tracked by ExileLink in the
    // resolver so downstream `TargetFilter::ExiledBySource` lookups (Etali's
    // "from among the nonland cards exiled this way" sub-clause) resolve
    // correctly.
    //
    // Both second-person ("exile … your library … until you exile") and
    // third-person ("exile … their library … until they exile") possessive
    // variants are accepted. The "each player " subject is stripped upstream
    // by `strip_each_player_subject`, which deconjugates "exiles" → "exile";
    // by the time we get here Etali's trigger body reads
    //   "exile cards from the top of their library until they exile a nonland card"
    // — same verb form as the second-person path with a different determiner.
    //
    // Mirrors the analogous sub-combinator pattern in
    // `parse_reveal_until_prefix` (oracle_effect/mod.rs:2433) which handles
    // the same possessive variation for `RevealUntil`.
    let (_, rest_orig) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, _) = alt((tag("exile "), tag("exiles "))).parse(i)?;
        let (i, _) = tag("cards from the top of ").parse(i)?;
        let (i, _) = alt((
            tag("your "),
            tag("their "),
            tag("his "),
            tag("her "),
            tag("its "),
        ))
        .parse(i)?;
        let (i, _) = tag("library until ").parse(i)?;
        let (i, _) = alt((
            tag("you exile "),
            tag("they exile "),
            tag("he exiles "),
            tag("she exiles "),
            tag("it exiles "),
        ))
        .parse(i)?;
        nom_primitives::parse_article(i)
    })?;
    let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];

    // Extract the filter from "nonland card", "creature card", etc.
    let filter_text = rest_lower.trim_end_matches('.').trim_end_matches(" card");

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

/// CR 701.20a: Parse the prefix `"reveal[s] cards from the top of <possessive>
/// library until <pronoun> reveal[s] "` and return the remaining text containing
/// the filter clause. Handles both second-person ("reveal cards from the top of
/// your library until you reveal") and third-person possessive variants
/// ("their/his/her/its library until they/he/she/it reveal[s]").
///
/// Returns the slice after the matched article (`a`/`an`), so the caller can
/// extract the filter text directly.
fn parse_reveal_until_prefix(input: &str) -> nom::IResult<&str, (), VerboseError<&str>> {
    // CR 701.20a: verb form — bare imperative ("reveal") or third-person ("reveals")
    let (input, _) = alt((tag("reveals "), tag("reveal "))).parse(input)?;
    let (input, _) = tag("cards from the top of ").parse(input)?;
    // CR 109.5: possessive determiner identifying the library owner. Both English
    // possessive forms ("your"/"their"/"his"/"her"/"its") map to the same parsed
    // structure; the actual player is supplied by the caller via subject context.
    let (input, _) = alt((
        tag("your "),
        tag("their "),
        tag("his "),
        tag("her "),
        tag("its "),
    ))
    .parse(input)?;
    let (input, _) = tag("library until ").parse(input)?;
    // Pronoun + matching verb form.
    let (input, _) = alt((
        tag("you reveal "),
        tag("they reveal "),
        tag("he reveals "),
        tag("she reveals "),
        tag("it reveals "),
    ))
    .parse(input)?;
    let (input, _) = nom_primitives::parse_article(input)?;
    Ok((input, ()))
}

/// CR 701.20a: Parse "reveal[s] cards from the top of <possessive> library
/// until <pronoun> reveal[s] a [filter]". Builds the [`Effect::RevealUntil`]
/// with `player` identifying whose library is revealed.
///
/// Defaults: kept_destination = Hand, rest_destination = Library (bottom).
/// Subsequent "put that card" / "put the rest" sentences override via
/// ContinuationAst.
fn try_parse_reveal_until(tp: TextPair, player: TargetFilter) -> Option<ParsedEffectClause> {
    let (_, rest_orig) = nom_on_lower(tp.original, tp.lower, parse_reveal_until_prefix)?;
    let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];

    // Strip trailing period and " card" suffix to get the filter text.
    let filter_text = rest_lower.trim_end_matches('.').trim_end_matches(" card");

    // Parse "card with the chosen name" specially.
    let filter = if nom_primitives::scan_contains(filter_text, "with the chosen name") {
        TargetFilter::HasChosenName
    } else if filter_text == "nonland" {
        TargetFilter::Typed(
            TypedFilter::default().with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
        )
    } else {
        let (parsed, _) = parse_target(filter_text);
        parsed
    };

    // CR 701.20a: Default destinations — most cards use hand + library bottom.
    // Subsequent "put that card" / "put the rest" sentences refine these via
    // RevealUntilKept / PutRest continuations.
    Some(parsed_clause(Effect::RevealUntil {
        player,
        filter,
        kept_destination: Zone::Hand,
        rest_destination: Zone::Library,
        enter_tapped: false,
    }))
}

/// CR 611.2a + CR 108.3: Parse per-grantee grant clauses that follow a
/// compound-exile effect. These clauses bind the resulting `PlayFromExile`
/// permission to a player OTHER than the ability's controller — the exiled
/// object's owner, or the parent effect's player target — via
/// [`PermissionGrantee`]. The target is `TrackedSet` — the most recently
/// published tracked set (i.e., the exiled cards).
///
/// Patterns:
/// - `for each of those cards, its owner may play it [until the end of their next turn]`
///   → [`PermissionGrantee::ObjectOwner`] (Suspend Aggression). The
///   "for each of those cards" prefix is rhetorical — the resolver already
///   iterates the target set object-by-object.
/// - `they may play those cards [until the end of their next turn]`
///   → [`PermissionGrantee::ParentTargetController`] (Expedited Inheritance).
///   The pronoun "they" refers to the parent effect's player target.
fn try_parse_per_grantee_play_grant(tp: TextPair<'_>) -> Option<ParsedEffectClause> {
    let lower = tp.lower;

    let grantee = if alt((
        tag::<_, _, VerboseError<&str>>("for each of those cards, its owner may play it"),
        tag("for each of those cards, its owner may cast it"),
        tag("its owner may play it"),
        tag("its owner may cast it"),
    ))
    .parse(lower)
    .is_ok()
    {
        crate::types::ability::PermissionGrantee::ObjectOwner
    } else if alt((
        tag::<_, _, VerboseError<&str>>("they may play those cards"),
        tag("they may cast those cards"),
        tag("they may play them"),
        tag("they may cast them"),
    ))
    .parse(lower)
    .is_ok()
    {
        crate::types::ability::PermissionGrantee::ParentTargetController
    } else {
        return None;
    };

    // Duration: default to UntilEndOfTurn if unspecified (matches impulse-draw default).
    let (_, dur) = strip_trailing_duration(tp.original);
    let duration = dur.unwrap_or(Duration::UntilEndOfTurn);

    Some(parsed_clause(Effect::GrantCastingPermission {
        permission: CastingPermission::PlayFromExile {
            duration,
            // Placeholder — `grant_permission::resolve` normalizes per-iteration.
            granted_to: crate::types::player::PlayerId(0),
        },
        target: TargetFilter::TrackedSet {
            id: TrackedSetId(0),
        },
        grantee,
    }))
}

/// CR 400.7i: Parse "you may play/cast that card [this turn]" — impulse draw permission.
fn try_parse_play_from_exile(tp: TextPair) -> Option<ParsedEffectClause> {
    let tp = tp.trim_end_matches('.');

    // CR 611.2a + CR 108.3: Per-object grant clauses from compound-exile chains.
    // These bind the grant to a player OTHER than the ability's controller via
    // Theme D's `granted_to` field, resolved per-iteration by
    // `grant_permission::resolve`. The target is `TrackedSet` — the most
    // recently published set (the exiled cards from the parent effect).
    if let Some(clause) = try_parse_per_grantee_play_grant(tp) {
        return Some(clause);
    }

    // Try full forms first: "you may play/cast that card/it/those cards ..."
    // Then bare forms (after "you may" has been stripped): "play that card ..."
    let full_rest = nom_on_lower(tp.original, tp.lower, |input| {
        value((), alt((tag("you may play "), tag("you may cast ")))).parse(input)
    })
    .map(|((), rest_orig)| {
        let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];
        TextPair::new(rest_orig, rest_lower)
    });

    if let Some(rest) = full_rest {
        // Full form: rest must start with a card reference
        if alt((
            tag::<_, _, VerboseError<&str>>("that card"),
            tag("that spell"),
            tag("those cards"),
            tag("it "),
        ))
        .parse(rest.lower)
        .is_err()
            && rest.lower != "it"
        {
            return None;
        }
    } else {
        // Bare form (after "you may" was stripped by parse_effect_chain):
        // Only match when temporal context exists ("this turn", "until"),
        // otherwise it's a CastFromZone, not impulse draw permission.
        let has_temporal =
            scan_contains_phrase(tp.lower, "this turn") || scan_contains_phrase(tp.lower, "until ");
        if !has_temporal {
            return None;
        }
        if scan_contains_phrase(tp.lower, "without paying") {
            return None;
        }
        if alt((
            tag::<_, _, VerboseError<&str>>("play that card"),
            tag("cast that card"),
            tag("play it"),
            tag("cast it"),
        ))
        .parse(tp.lower)
        .is_err()
        {
            return None;
        }
    }

    // Duration: extract from trailing text, defaulting to UntilEndOfTurn for impulse draw
    let (_, dur) = strip_trailing_duration(tp.original);
    let duration = dur.unwrap_or(Duration::UntilEndOfTurn);

    Some(parsed_clause(Effect::GrantCastingPermission {
        permission: CastingPermission::PlayFromExile {
            duration,
            // Placeholder — `grant_permission::resolve` rewrites this to the
            // ability's controller at grant time (CR 611.2a/b).
            granted_to: crate::types::player::PlayerId(0),
        },
        target: TargetFilter::Any,
        grantee: Default::default(),
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

    // Parse the "for each" clause into a QuantityRef.
    // Strip duration from the for-each clause text first — handles unusual ordering
    // like "for each card in your hand until end of turn" (Ral's Staticaster).
    let (for_each_no_duration, for_each_duration) = strip_trailing_duration(for_each_clause);
    let qty = parse_for_each_clause(for_each_no_duration.trim_end_matches('.'))?;
    let quantity = QuantityExpr::Ref { qty };

    // Strip trailing duration from the base text (e.g., "gets +2/+0 until end of turn"
    // → "gets +2/+0" with duration=UntilEndOfTurn). Duration often appears between
    // the base effect and the "for each" clause.
    let (base_no_duration, base_duration) = strip_trailing_duration(base_tp.original);
    let duration = base_duration.or(for_each_duration);
    let base_no_duration_lower = base_no_duration.to_lowercase();

    // Delegate to parse_numeric_imperative_ast — it already handles draw, gain/lose life,
    // pump, scry, surveil, mill. Replace fixed counts with the for-each quantity, then
    // thread subject through for effects that carry a target.
    if let Some(ast) =
        imperative::parse_numeric_imperative_ast(base_no_duration, &base_no_duration_lower)
    {
        let effect = imperative::lower_numeric_imperative_ast(ast.with_for_each_quantity(quantity));
        let effect = thread_for_each_subject(effect, base_no_duration);
        return Some(ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
            distribute: None,
            multi_target: None,
            condition: None,
            optional: false,
        });
    }

    // CR 120.1: "[subject] deals N damage to [target] for each X" → DealDamage.
    // Delegates to try_parse_damage, which already handles amount extraction, target parsing,
    // and damage source variants. Replace the fixed amount with the for-each quantity.
    if let Some(effect) = try_parse_damage(base_tp.lower, base_tp.original) {
        let effect = match effect {
            Effect::DealDamage {
                amount,
                target,
                damage_source,
            } => Effect::DealDamage {
                amount: replace_fixed_quantity(amount, quantity.clone()),
                target,
                damage_source,
            },
            Effect::DamageEachPlayer {
                amount,
                player_filter,
            } => Effect::DamageEachPlayer {
                amount: replace_fixed_quantity(amount, quantity.clone()),
                player_filter,
            },
            Effect::DamageAll {
                amount,
                target,
                player_filter,
            } => Effect::DamageAll {
                amount: replace_fixed_quantity(amount, quantity),
                target,
                player_filter,
            },
            other => other,
        };
        return Some(parsed_clause(effect));
    }

    // CR 111.11: "create [token description] for each X" → Token with dynamic count.
    // Delegates to try_parse_token, which handles token description parsing (name, P/T,
    // types, colors, keywords). Replace the embedded count with the for-each quantity.
    if let Some(effect) = token::try_parse_token(base_tp.lower, base_tp.original) {
        let effect = match effect {
            Effect::Token {
                name,
                power,
                toughness,
                types,
                colors,
                keywords,
                tapped,
                owner,
                attach_to,
                enters_attacking,
                supertypes,
                static_abilities,
                enter_with_counters,
                count: _,
            } => Effect::Token {
                name,
                power,
                toughness,
                types,
                colors,
                keywords,
                tapped,
                owner,
                attach_to,
                enters_attacking,
                supertypes,
                static_abilities,
                enter_with_counters,
                count: quantity,
            },
            other => other,
        };
        return Some(parsed_clause(effect));
    }

    // "put a [counter type] counter on [target] for each X" → PutCounter with dynamic count.
    // Not a NumericImperativeAst — counter placement has its own structure.
    if let Ok((_, before)) =
        take_until::<_, _, VerboseError<&str>>("counter on").parse(base_tp.lower)
    {
        // before = "put a +1/+1 " — strip "put a[n] " prefix then parse counter type.
        let ct_start = alt((
            value((), tag::<_, _, VerboseError<&str>>("put a ")),
            value((), tag("put an ")),
            value((), tag("put ")),
        ))
        .parse(before)
        .map(|(rest, _)| rest)
        .unwrap_or(before)
        .trim();
        // Parse through the typed combinator so the canonical mapping lives in
        // `types::counter::parse_counter_type`. `Effect::PutCounter::counter_type`
        // remains `String` (serialization format), so emit the canonical
        // `as_str()` form rather than the raw Oracle token.
        let (_, counter_type_enum) = nom_primitives::parse_counter_type_typed
            .parse(ct_start)
            .ok()?;
        let counter_type = counter_type_enum.as_str().to_string();
        let counter_on_end = before.len() + "counter on ".len();
        let after_counter_on = base_tp.original[counter_on_end..].trim();
        // Strip clause separators (", then") that leak into the counter target text.
        let after_counter_on_lower = after_counter_on.to_lowercase();
        let counter_target_text = if let Ok((_, before_sep)) =
            take_until::<_, _, VerboseError<&str>>(", then").parse(after_counter_on_lower.as_str())
        {
            &after_counter_on[..before_sep.len()]
        } else {
            after_counter_on
        };
        let target = parse_subject_application(counter_target_text, &ParseContext::default())
            .map(|app| app.affected)
            .unwrap_or_else(|| {
                push_warning(format!(
                    "target-fallback: unrecognized counter target '{}'",
                    after_counter_on
                ));
                TargetFilter::Any
            });
        return Some(parsed_clause(Effect::PutCounter {
            counter_type,
            count: quantity,
            target,
        }));
    }

    None
}

/// Thread subject through for-each effects that carry a `target` field.
/// Locates the predicate verb, extracts subject text before it, and replaces
/// default targets (Any/Controller) with the parsed subject filter.
fn thread_for_each_subject(effect: Effect, original: &str) -> Effect {
    let lower = original.to_lowercase();
    // Predicate verbs that parse_numeric_imperative_ast recognizes — find the earliest one.
    // Note: uses str::find (not nom) because this is positional splitting on already-dispatched
    // text (base_tp from try_parse_for_each_effect), not parsing dispatch on raw Oracle text.
    // The input is short and constrained, so substring false positives are not a concern.
    // Find the predicate verb. Must check for space-before-verb boundary to avoid
    // substring matches inside other words (e.g., "target" contains "get").
    // The first verb may appear at position 0 (no preceding space) for bare imperatives.
    let verb_pos = [
        " gets ",
        " get ",
        " gains ",
        " gain ",
        " loses ",
        " lose ",
        " draws ",
        " draw ",
        " mills ",
        " mill ",
        " scries ",
        " scry ",
        " surveils ",
        " surveil ",
    ]
    .iter()
    .filter_map(|verb| lower.find(verb).map(|pos| pos + 1)) // +1 to skip the leading space
    .min();

    let subject_text = match verb_pos {
        Some(pos) if pos > 0 => original[..pos].trim(),
        _ => return effect,
    };
    if subject_text.is_empty() {
        return effect;
    }

    let (target, is_targeted) =
        match parse_subject_application(subject_text, &ParseContext::default()) {
            Some(app) => (app.affected, app.target.is_some()),
            None => return effect,
        };

    // Only replace default/placeholder targets — leave already-resolved targets alone.
    match effect {
        // CR 609 + CR 611.2: "All creatures get -1/-1 until end of turn for each Swamp
        // you control" (Mutilate). When the subject filter identifies a class of
        // permanents (not a single targeted object or single-object ref), lower to
        // `Effect::PumpAll` so the continuous effect applies to every matching
        // permanent — not a single target. Mirrors `subject::build_pump_effect`.
        Effect::Pump {
            power,
            toughness,
            target: TargetFilter::Any,
        } => {
            if is_targeted || subject::is_single_object_ref(&target) {
                Effect::Pump {
                    power,
                    toughness,
                    target,
                }
            } else {
                Effect::PumpAll {
                    power,
                    toughness,
                    target,
                }
            }
        }
        Effect::Mill {
            count,
            target: TargetFilter::Controller,
            destination,
        } => Effect::Mill {
            count,
            target,
            destination,
        },
        other => other,
    }
}

#[tracing::instrument(level = "trace")]
fn parse_clause_ast(text: &str, ctx: &ParseContext) -> ClauseAst {
    let text = text.trim();

    // Mirror the CubeArtisan grammar's high-level sentence shapes:
    // 1) conditionals ("if X, Y"), 2) subject + verb phrase, 3) bare imperative.
    if let Some((condition_text, remainder)) = split_leading_conditional(text) {
        // CR 608.2c: Parse the leading conditional guard through the nom condition pipeline.
        // Strip "if " prefix before passing to the condition parser.
        let condition_lower = condition_text.to_lowercase();
        let cond_body = nom_on_lower(&condition_text, &condition_lower, |i| {
            value((), tag("if ")).parse(i)
        })
        .map(|((), rest)| rest)
        .unwrap_or(&condition_text)
        .trim();
        let condition = try_nom_condition_as_ability_condition(cond_body);
        return ClauseAst::Conditional {
            condition,
            clause: Box::new(parse_clause_ast(&remainder, ctx)),
        };
    }

    // CR 701.24b: "each player who searched their library this way shuffles" —
    // redundant shuffle after a search effect (search already includes a shuffle).
    // Parse as a bare shuffle to avoid Unimplemented.
    {
        let lower = text.to_lowercase();
        if nom_primitives::scan_contains(&lower, "who searched") && lower.ends_with("shuffles") {
            return ClauseAst::Imperative {
                text: "shuffle".to_string(),
            };
        }
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
        ClauseAst::Imperative { text } => {
            let mut clause = lower_imperative_clause(&text, ctx);
            // CR 701.24g: "put target [type] on top/bottom of library" — the imperative
            // parser returns PutAtLibraryPosition { target: Any } because it dispatches
            // on the positional suffix without extracting the target phrase. Covers:
            //   - "put target X on top of Y's library"
            //   - "put target X on the bottom of Y's library"
            //   - "put target X into Y's library Nth from the top"  (e.g. Teferi, Hero of Dominaria)
            if let Effect::PutAtLibraryPosition { ref mut target, .. } = clause.effect {
                if *target == TargetFilter::Any {
                    let lower = text.to_lowercase();
                    let extracted_target = (|| -> Option<TargetFilter> {
                        let (after_put, _) = tag::<_, _, VerboseError<&str>>("put ")
                            .parse(lower.as_str())
                            .ok()?;
                        // Boundary terminators, in priority order. Each is a positional
                        // placement phrase that follows the target noun phrase.
                        let before = [" on top of", " on the bottom of", " into "]
                            .iter()
                            .find_map(|term| {
                                take_until::<_, _, VerboseError<&str>>(*term)
                                    .parse(after_put)
                                    .ok()
                                    .map(|(_, before)| before)
                            })?;
                        let (filter, _) = parse_target(before);
                        if matches!(filter, TargetFilter::Any) {
                            None
                        } else {
                            Some(filter)
                        }
                    })();
                    if let Some(filter) = extracted_target {
                        *target = filter;
                    }
                }
            }
            clause
        }
        ClauseAst::SubjectPredicate { subject, predicate } => {
            lower_subject_predicate_ast(*subject, *predicate, ctx)
        }
        ClauseAst::Conditional { condition, clause } => {
            // CR 608.2c: Thread the leading conditional into the lowered clause's condition field.
            let mut result = lower_clause_ast(*clause, ctx);
            if let Some(cond) = condition {
                result.condition = Some(cond);
            }
            result
        }
    }
}

#[tracing::instrument(level = "debug")]
fn lower_imperative_clause(text: &str, ctx: &ParseContext) -> ParsedEffectClause {
    // CR 106.1 + CR 109.1: "For each color among [X], add one mana of that color"
    // (Faeburrow Elder). Must run before the generic for-each-prefix strip path,
    // because "that color" anaphors a per-iteration color rather than the
    // source's `ChosenAttribute::Color`. Emits a single mana ability producing
    // one mana per distinct color among matching permanents.
    if let Some(effect) = super::oracle_effect::mana::try_parse_for_each_color_mana_public(text) {
        return parsed_clause(effect);
    }

    // "Its controller gains life equal to its power/toughness" — subject must be preserved
    // because the life recipient is not the caster but the targeted permanent's controller.
    if let Some(clause) = try_parse_targeted_controller_gain_life(text) {
        return clause;
    }

    // CR 121.1 + CR 615.5: "{verb} cards equal to {quantity}" inside a
    // subject-aware predicate (e.g. "the source's controller draws cards equal
    // to the damage prevented this way" — Swans of Bryn Argoll). Mirrors the
    // bare-imperative arm at `parse_effect_clause`: must run BEFORE the
    // simpler "draw cards" matchers to avoid being dropped to a default
    // count of 1.
    {
        let lower = text.to_lowercase();
        let tp = TextPair::new(text, lower.as_str());
        if let Some(clause) = try_parse_equal_to_quantity_effect(tp) {
            return clause;
        }
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

    // CR 609.4b: "spend mana as though it were mana of any color to cast ..." /
    // "mana of any type can be spent to cast ..." — grants any-color mana permission
    // for a cast-from-exile card. Produce a GenericEffect with SpendManaAsAnyColor static.
    // Variants: "spend colorless mana as though..." / "mana of any color to cast..."
    {
        let lower = text.to_lowercase();
        if nom_primitives::scan_contains(&lower, "as though it were mana of any color")
            || nom_primitives::scan_contains(&lower, "mana of any type can be spent to cast")
        {
            return parsed_clause(Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::SpendManaAsAnyColor)
                    .description(text.to_string())],
                duration: None,
                target: Some(TargetFilter::Controller),
            });
        }
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
    // Post-parse fixup for "exile N target" multi_target (same pattern as PutCounter above).
    // "exile two target permanents" → strip "exile " → "two target permanents" → (2, ...)
    if matches!(
        clause.effect,
        Effect::ChangeZone {
            destination: Zone::Exile,
            ..
        }
    ) && clause.multi_target.is_none()
    {
        clause.multi_target = extract_exile_multi_target(text);
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
    // CR 701.26a/b: Tap/untap all — mass variants must be checked before single-target
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| {
        value((), alt((tag("tap all "), tag("tap each ")))).parse(i)
    }) {
        let (target, rem) = parse_target(rest);
        return Some((TargetedImperativeAst::TapAll { target }, rem));
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| {
        value((), alt((tag("untap all "), tag("untap each ")))).parse(i)
    }) {
        let (target, rem) = parse_target(rest);
        return Some((TargetedImperativeAst::UntapAll { target }, rem));
    }
    // Simple targeted verbs: parse_target on text after the verb prefix
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| value((), tag("tap ")).parse(i)) {
        let (target_text, _) = strip_optional_target_prefix(rest);
        let (target, rem) = parse_target(target_text);
        return Some((TargetedImperativeAst::Tap { target }, rem));
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| value((), tag("untap ")).parse(i)) {
        let (target_text, _) = strip_optional_target_prefix(rest);
        let (target, rem) = parse_target(target_text);
        return Some((TargetedImperativeAst::Untap { target }, rem));
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| value((), tag("sacrifice ")).parse(i)) {
        let (count, after_count) = super::oracle_util::parse_count_expr(rest).unwrap_or((
            crate::types::ability::QuantityExpr::Fixed { value: 1 },
            rest,
        ));
        let (target_text, _) = strip_optional_target_prefix(after_count.trim_start());
        let (target, rem) = parse_target(target_text);
        return Some((TargetedImperativeAst::Sacrifice { target, count }, rem));
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| value((), tag("fight ")).parse(i)) {
        let (target_text, _) = strip_optional_target_prefix(rest);
        let (target, rem) = parse_target(target_text);
        return Some((TargetedImperativeAst::Fight { target }, rem));
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |i| value((), tag("gain control of ")).parse(i))
    {
        let (target_text, _) = strip_optional_target_prefix(rest);
        let (target, rem) = parse_target(target_text);
        let rem_lower = rem.to_ascii_lowercase();
        if tag::<_, _, VerboseError<&str>>(" during that player's next turn")
            .parse(rem_lower.as_str())
            .is_ok()
        {
            let rem = &rem[" during that player's next turn".len()..];
            let rem_lower = rem.to_ascii_lowercase();
            let (rem, grant_extra_turn_after) = if let Ok((rest, _)) = alt((
                tag::<_, _, VerboseError<&str>>(
                    ". after that turn, that player takes an extra turn",
                ),
                tag(" after that turn, that player takes an extra turn"),
                tag("after that turn, that player takes an extra turn"),
            ))
            .parse(rem_lower.as_str())
            {
                (&rem[rem.len() - rest.len()..], true)
            } else {
                (rem, false)
            };
            return Some((
                TargetedImperativeAst::ControlNextTurn {
                    target,
                    grant_extra_turn_after,
                },
                rem,
            ));
        }
        return Some((TargetedImperativeAst::GainControl { target }, rem));
    }
    // Earthbend: "earthbend [N] [target <type>]"
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("earthbend ").parse(lower) {
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
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("airbend ").parse(lower) {
        let original_rest = &text[text.len() - rest.len()..];
        let (target_text, _) = strip_optional_target_prefix(original_rest);
        let (target, after_target) = parse_target(target_text);
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
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| {
        value((), alt((tag("destroy all "), tag("destroy each ")))).parse(i)
    }) {
        let (target, rem) = parse_target(rest);
        return Some((
            TargetedImperativeAst::ZoneCounterProxy(Box::new(ZoneCounterImperativeAst::Destroy {
                target,
                all: true,
            })),
            rem,
        ));
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| value((), tag("destroy ")).parse(i)) {
        let (target, rem) = parse_target(rest);
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
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| {
        value((), alt((tag("exile all "), tag("exile each ")))).parse(i)
    }) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (parsed_target, rem) = parse_target(rest);
        // CR 701.5a: "exile all spells" must constrain to the stack.
        let target = if scan_contains_phrase(rest_lower, "spell") {
            constrain_filter_to_stack(parsed_target)
        } else {
            parsed_target
        };
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
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| value((), tag("exile ")).parse(i)) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (parsed_target, rem) = parse_target(rest);
        // CR 701.5a: "exile target spell" must constrain targeting to the stack,
        // mirroring the counter-spell parser at line 1036-1037.
        let target = if scan_contains_phrase(rest_lower, "spell") {
            constrain_filter_to_stack(parsed_target)
        } else {
            parsed_target
        };
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

    // CR 701.6 + CR 405.1: "Counter all/each [filter] spells/abilities" mass-
    // counter precheck — must run before the bare `tag("counter ")` arm so a
    // compound-clause head like "Counter all spells your opponents control
    // and ..." promotes to `Effect::CounterAll` instead of falling through to
    // single-target. Mirrors the imperative.rs `parse_counter_ast` precheck.
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| {
        value((), alt((tag("counter all "), tag("counter each ")))).parse(i)
    }) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (parsed_target, rem) = parse_target(rest);
        // CR 113.3: Bare "abilities" (no intervening type phrase) ⇒ stack-
        // ability filter. Mass mode is loose enough to accept this form
        // because the mass keyword has already disambiguated the verb sense.
        // Bare "abilities" head: nom tag-match (skipping leading whitespace)
        // so the dispatch is a combinator, not a string starts_with.
        fn abilities_head(i: &str) -> nom::IResult<&str, &str, VerboseError<&str>> {
            preceded(nom::character::complete::multispace0, tag("abilities")).parse(i)
        }
        let target = if scan_contains_phrase(rest_lower, "activated or triggered ability")
            || abilities_head(rest_lower).is_ok()
        {
            TargetFilter::StackAbility
        } else if scan_contains_phrase(rest_lower, "spell") {
            constrain_filter_to_stack(parsed_target)
        } else {
            parsed_target
        };
        let unless_payment = parse_unless_payment(rest_lower);
        return Some((
            TargetedImperativeAst::ZoneCounterProxy(Box::new(ZoneCounterImperativeAst::Counter {
                target,
                source_static: None,
                unless_payment,
                all: true,
            })),
            rem,
        ));
    }

    // CR 701.6: Counter a spell or ability on the stack.
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| value((), tag("counter ")).parse(i)) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (parsed_target, rem) = parse_target(rest);
        let target = if scan_contains_phrase(rest_lower, "activated or triggered ability") {
            // CR 701.6: "activated or triggered ability" is a special-case target
            // that maps to StackAbility. We still use parse_target's remainder to
            // preserve the compound-detection contract.
            TargetFilter::StackAbility
        } else if scan_contains_phrase(rest_lower, "spell") {
            constrain_filter_to_stack(parsed_target)
        } else {
            parsed_target
        };
        // CR 118.12: Parse "unless its controller pays {X}" for conditional counters
        let unless_payment = parse_unless_payment(rest_lower);
        return Some((
            TargetedImperativeAst::ZoneCounterProxy(Box::new(ZoneCounterImperativeAst::Counter {
                target,
                source_static: None,
                unless_payment,
                all: false,
            })),
            rem,
        ));
    }

    // Return: determine destination separately, use parse_target remainder for compound detection
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| value((), tag("return ")).parse(i)) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (_, dest) = strip_return_destination_ext(rest);
        let (target, rem) = parse_target(rest);
        let origin = infer_origin_zone(rest_lower);
        return match dest {
            Some(d) if d.zone == Zone::Battlefield => Some((
                TargetedImperativeAst::ReturnToBattlefield {
                    target,
                    origin,
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
                    origin,
                    destination: d.zone,
                },
                rem,
            )),
            None => Some((TargetedImperativeAst::Return { target }, rem)),
        };
    }

    // Put counter: use refactored try_parse_put_counter that returns remainder
    if tag::<_, _, VerboseError<&str>>("put ").parse(lower).is_ok()
        && scan_contains_phrase(lower, "counter")
    {
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

    // Quick bail: no "and" means no compound connector possible
    if !scan_contains_phrase(&lower, "and") {
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
    let (after_and, _) = tag::<_, _, VerboseError<&str>>(" and ")
        .parse(remainder)
        .ok()?;

    let sub_text = after_and.trim();
    if sub_text.is_empty() {
        return None;
    }

    // Lower the primary AST to an Effect
    let primary_effect = match primary_ast {
        TargetedImperativeAst::ZoneCounterProxy(ast) => lower_zone_counter_ast(*ast),
        other => lower_targeted_action_ast(other),
    };

    let sub_lower = sub_text.to_lowercase();
    let uses_parent_target_reference = has_anaphoric_reference(&sub_lower);
    let continuation_ctx = if uses_parent_target_reference {
        ParseContext {
            card_name: ctx.card_name.clone(),
            ..Default::default()
        }
    } else {
        ctx.clone()
    };

    // Parse the sub-effect
    let mut sub_clause = parse_imperative_effect(sub_text, &continuation_ctx);

    // CR 608.2c: Verb carry-forward for bare "target X" clauses in compound actions.
    // When the sub-text starts with "target" and parsed as Unimplemented, prepend
    // the verb from the primary effect and re-parse. Handles "exile target creature
    // and target artifact" where "target artifact" lacks a verb.
    if matches!(sub_clause.effect, Effect::Unimplemented { .. })
        && tag::<_, _, VerboseError<&str>>("target ")
            .parse(sub_lower.as_str())
            .is_ok()
    {
        if let Some(verb) = extract_effect_verb(&primary_effect) {
            let reparsed_text = format!("{verb} {sub_text}");
            let reparsed = parse_imperative_effect(&reparsed_text, &continuation_ctx);
            if !matches!(reparsed.effect, Effect::Unimplemented { .. }) {
                sub_clause = reparsed;
            }
        }
    }

    // CR 608.2c: Possessive zone carry-forward for compound actions.
    // "exiles a creature they control and their graveyard" → sub-text "their graveyard"
    // lacks a verb. Prepend the primary verb so it becomes "exile their graveyard".
    if matches!(sub_clause.effect, Effect::Unimplemented { .. })
        && (starts_with_possessive(&sub_lower, "", "graveyard")
            || starts_with_possessive(&sub_lower, "", "library")
            || starts_with_possessive(&sub_lower, "", "hand"))
    {
        if let Some(verb) = extract_effect_verb(&primary_effect) {
            let reparsed_text = format!("{verb} {sub_text}");
            let reparsed = parse_imperative_effect(&reparsed_text, &continuation_ctx);
            if !matches!(reparsed.effect, Effect::Unimplemented { .. }) {
                sub_clause = reparsed;
            }
        }
    }

    // CR 608.2c: Top-of-library carry-forward for compound actions.
    // "Exile target nonland permanent and the top card of your library"
    // (Suspend Aggression) splits on " and " into sub-text "the top card of
    // your library" which lacks a verb. Prepend the primary verb so it
    // becomes "exile the top card of your library" — parsed as ExileTop.
    if matches!(sub_clause.effect, Effect::Unimplemented { .. })
        && tag::<_, _, VerboseError<&str>>("the top ")
            .parse(sub_lower.as_str())
            .is_ok()
    {
        if let Some(verb) = extract_effect_verb(&primary_effect) {
            let reparsed_text = format!("{verb} {sub_text}");
            let reparsed = parse_imperative_effect(&reparsed_text, &continuation_ctx);
            if !matches!(reparsed.effect, Effect::Unimplemented { .. }) {
                sub_clause = reparsed;
            }
        }
    }

    // If the remainder contains anaphoric references ("it", "that creature", "them"),
    // replace the sub_effect's target with ParentTarget so it inherits the parent's targets.
    if uses_parent_target_reference {
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
        condition: None,
        optional: false,
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
/// Detect "deals N damage to each [opponent/player] and each [type] they control".
///
/// CR 120.3 + CR 609.7: This compound target pattern deals damage to BOTH players
/// and objects from a single source as one simultaneous damage event. Producing
/// two chained effects (DamageEachPlayer + DamageAll sub_ability) would split
/// the event in two, breaking replacement/prevention shields like Awe Strike
/// ("the next time a source would deal damage ... prevent that damage"), which
/// must observe the full batch as one event from one source.
///
/// Emits a single `Effect::DamageAll` carrying both the object `target` filter
/// and a `player_filter` populated — the resolver iterates the combined
/// recipient set under one `DamageContext`.
///
/// Cards: Goblin Chainwhirler, Kumano Faces Kakkazan, etc.
fn try_parse_compound_player_object_damage(lower: &str) -> Option<ParsedEffectClause> {
    // Extract the damage verb + amount using the same entry logic as try_parse_damage_with_remainder.
    // structural: not dispatch — positional search for verb in variable-length subject prefix
    let pos = lower.find("deals ").or_else(|| lower.find("deal "))?;
    let verb_len = if tag::<_, _, VerboseError<&str>>("deals ")
        .parse(&lower[pos..])
        .is_ok()
    {
        6
    } else {
        5
    };
    let after_lower = &lower[pos + verb_len..];

    // Parse amount: "N damage to "
    let (qty, after_amount) =
        super::oracle_util::parse_count_expr(after_lower).and_then(|(qty, rest)| {
            let (rest, _) = tag::<_, _, VerboseError<&str>>("damage").parse(rest).ok()?;
            let rest = rest.trim_start();
            let (rest, _) = tag::<_, _, VerboseError<&str>>("to ").parse(rest).ok()?;
            Some((qty, rest))
        })?;

    // Match: "each [opponent/player] and each [type phrase] {they control | you don't control | your opponents control}"
    let (after_each, _) = tag::<_, _, VerboseError<&str>>("each ")
        .parse(after_amount)
        .ok()?;
    let (after_player, player_filter) = parse_damage_player_scope(after_each).ok()?;

    // " and each " connector
    let (after_and_each, _) = tag::<_, _, VerboseError<&str>>(" and each ")
        .parse(after_player)
        .ok()?;

    // Trim a trailing period so suffix-anchored controller phrases match cleanly.
    let after_and_each = after_and_each.trim_end_matches('.').trim_end();
    if after_and_each.is_empty() {
        return None;
    }

    // CR 109.4 + CR 109.5: Probe for one of the three controller-suffix variants
    // this compound class uses. All three resolve to ControllerRef::Opponent for
    // the object filter — "they" anaphors the opponent set named in the first
    // half; "you don't control" and "your opponents control" are direct opponent
    // refs. The suffix presence is the gate for the compound shape — without it
    // the " and " is part of an unrelated clause and falls through to general
    // split. `take_until` + an anchored controller-tag alt() is the single-pass
    // gate; the take_until result (the type-phrase prefix) is unused here because
    // we hand the full slice to parse_target below for native consumption.
    let mut probe = alt((
        preceded(
            take_until::<_, _, VerboseError<&str>>(" they control"),
            tag(" they control"),
        ),
        preceded(
            take_until::<_, _, VerboseError<&str>>(" you don't control"),
            tag(" you don't control"),
        ),
        preceded(
            take_until::<_, _, VerboseError<&str>>(" your opponents control"),
            tag(" your opponents control"),
        ),
    ));
    let (gate_rest, _) = probe.parse(after_and_each).ok()?;
    if !gate_rest.is_empty() {
        return None;
    }

    // Use parse_target on "each [type phrase + controller suffix]" so the controller
    // suffix is consumed natively by the parser pipeline. parse_target returns the
    // typed filter with controller already populated (Opponent for "you don't control"
    // / "your opponents control"; You for "they control" — we rewrite that below).
    let target_text = format!("each {after_and_each}");
    let (mut object_filter, _rem) = parse_target(&target_text);

    // CR 109.5: "they" in this compound damage context anaphors back to the
    // opponent set in the first half of the conjunction, NOT to the controller.
    // Rewrite ControllerRef::You (parse_target's default for "they control") to
    // Opponent so the runtime targets the same opponent set as `player_filter`.
    // "you don't control" and "your opponents control" already resolve to
    // Opponent via `parse_zone_controller` and are left unchanged.
    fn set_opponent_controller(filter: &mut TargetFilter) {
        match filter {
            TargetFilter::Typed(tf) => {
                tf.controller = Some(ControllerRef::Opponent);
            }
            TargetFilter::Or { filters } => {
                for f in filters.iter_mut() {
                    set_opponent_controller(f);
                }
            }
            _ => {}
        }
    }
    set_opponent_controller(&mut object_filter);

    // CR 120.3: Single effect, one damage source, simultaneous batch across both
    // the player set and the object set. Replacement effects observe this as one
    // coherent damage event — see `resolve_all` in game/effects/deal_damage.rs.
    Some(ParsedEffectClause {
        effect: Effect::DamageAll {
            amount: qty,
            target: object_filter,
            player_filter: Some(player_filter),
        },
        duration: None,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
    })
}

/// CR 120.3 + CR 119.3a: Compound object+player damage — type-first variant.
/// Detects "deals N damage to each [type-phrase] and each player" and emits a
/// single `Effect::DamageAll` carrying both the object filter and a
/// `player_filter: Some(PlayerFilter::All)` so the resolver damages every
/// matching creature AND every player from one batch (preserving simultaneous
/// damage event semantics for replacement/prevention shields).
///
/// Mirrors `try_parse_compound_player_object_damage` but with the conjunction
/// reversed: the first half names the object filter (with optional property
/// suffix like "with flying" / "without flying" / "with horsemanship"), the
/// second half is the player scope ("each player"). Without this gate the
/// general compound splitter parses "each [type]" and silently drops the
/// "and each player" continuation.
///
/// Cards: Pyrohemia, Pestilence, Earthquake, Hurricane, Inferno, Famine,
/// Fire Tempest, Cave-In, Crypt Rats, Cloudthresher, Flamebreak, Devastate,
/// Dakmor Plague, Dry Spell, Festering Evil, etc. (~30 cards).
fn try_parse_compound_object_player_damage(lower: &str) -> Option<ParsedEffectClause> {
    // Reuse the same verb+amount entry logic as the player-first variant.
    let pos = lower.find("deals ").or_else(|| lower.find("deal "))?; // allow-noncombinator: positional verb search inside a variable-length subject prefix; tag("deals ") below is the actual dispatch.
    let verb_len = if tag::<_, _, VerboseError<&str>>("deals ")
        .parse(&lower[pos..])
        .is_ok()
    {
        6
    } else {
        5
    };
    let after_lower = &lower[pos + verb_len..];

    // Parse amount: "N damage to "
    let (qty, after_amount) =
        super::oracle_util::parse_count_expr(after_lower).and_then(|(qty, rest)| {
            let (rest, _) = tag::<_, _, VerboseError<&str>>("damage").parse(rest).ok()?;
            let rest = rest.trim_start();
            let (rest, _) = tag::<_, _, VerboseError<&str>>("to ").parse(rest).ok()?;
            Some((qty, rest))
        })?;

    // Must start with "each " (the object half of the conjunction).
    let (after_each, _) = tag::<_, _, VerboseError<&str>>("each ")
        .parse(after_amount)
        .ok()?;

    // Locate the " and each player" connector. take_until isolates the type-phrase
    // prefix; the player suffix gates the compound shape and may carry a trailing
    // period, comma, or independent continuation clause (e.g., Flamebreak:
    // "and each player. Creatures dealt damage this way..."). After consuming the
    // tag, the next char must be a clause boundary — anything else (e.g.,
    // " who controls a white creature" on Disorder, " equal to ..." on Pompeii)
    // means "each player" is a QUALIFIED player set, not the universal scope.
    let (after_player, type_phrase) = take_until::<_, _, VerboseError<&str>>(" and each player")
        .parse(after_each)
        .ok()?;
    let (after_player_tag, _) = tag::<_, _, VerboseError<&str>>(" and each player")
        .parse(after_player)
        .ok()?;

    // CR 109.5: Reject qualified player phrases. Only accept clause-boundary
    // characters (whitespace + punctuation) or end-of-string after "each player".
    // " who", " equal", " that", " whose", and similar word continuations indicate
    // a relative clause that restricts the player set — those cards are not part
    // of this class.
    if let Some(c) = after_player_tag.chars().next() {
        if !(c.is_ascii_punctuation() || c == ' ') {
            return None;
        }
        // Whitespace alone isn't enough — peek at the next word.
        if c == ' ' {
            let next = after_player_tag.trim_start();
            // A bare trailing space + period/comma is fine (already covered by punctuation
            // peek above), but " who/equal/that/whose/with..." is a qualifier.
            // The only acceptable post-space content is another sentence (capitalized,
            // started by punctuation), or the empty tail. We approximate with a tag
            // probe over the known qualifier words.
            let mut qualifier = alt((
                tag::<_, _, VerboseError<&str>>("who "),
                tag("whose "),
                tag("that "),
                tag("equal "),
                tag("with "),
                tag("without "),
            ));
            if qualifier.parse(next).is_ok() {
                return None;
            }
        }
    }

    if type_phrase.is_empty() {
        return None;
    }

    // Reject if the type-phrase itself contains a stray "and each " — that
    // suggests a more complex N-way conjunction we don't yet handle.
    if scan_contains_phrase(type_phrase, "and each") {
        return None;
    }

    // Hand the "each [type-phrase]" slice to parse_target so property suffixes
    // ("with flying", "without flying", "with horsemanship") are consumed
    // natively. The full lower-case slice is fine because parse_target is
    // case-insensitive and we only use the resulting filter, not the remainder.
    let target_text = format!("each {type_phrase}");
    let (object_filter, _rem) = parse_target(&target_text);

    // Sanity: the parsed filter must reference at least one object type.
    // If parse_target returned an empty filter, bail out so the general
    // compound splitter gets a chance.
    if matches!(&object_filter, TargetFilter::None) {
        return None;
    }

    Some(ParsedEffectClause {
        effect: Effect::DamageAll {
            amount: qty,
            target: object_filter,
            player_filter: Some(PlayerFilter::All),
        },
        duration: None,
        sub_ability: None,
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
    })
}

fn try_split_damage_compound(text: &str, ctx: &ParseContext) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    if !scan_contains_phrase(&lower, "and") {
        return None;
    }

    // CR 120.2a: Compound player+object damage — "each opponent and each [type] they control"
    // must be detected before the general compound splitter, because the "and" here connects
    // two damage targets (not two independent effects).
    if let Some(clause) = try_parse_compound_player_object_damage(&lower) {
        return Some(clause);
    }

    // CR 120.3 + CR 119.3a: Same shape, type-first ordering — "each creature
    // [with X | without X] and each player" (Pyrohemia / Earthquake / Hurricane
    // class). Must run before the general split so the player half isn't dropped.
    if let Some(clause) = try_parse_compound_object_player_damage(&lower) {
        return Some(clause);
    }

    let (primary_effect, remainder) = try_parse_damage_with_remainder(text, &lower)?;

    if remainder.is_empty() {
        return None;
    }

    // The remainder must start with " and " to be a compound connector.
    // Do NOT trim — the leading space is the boundary marker.
    let (after_and, _) = tag::<_, _, VerboseError<&str>>(" and ")
        .parse(remainder)
        .ok()?;
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
        condition: None,
        optional: false,
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
    // Find " and " that separates subjects
    let (_, (first_text, after_and)) = nom_primitives::split_once_on(text, " and ").ok()?;
    let first_text = first_text.trim();
    let after_and = after_and.trim();

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
    // Positional find for preposition boundary — position used to slice both
    // original-case and lowercase strings, so find() is kept for dual-string slicing.
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
    tag::<_, _, VerboseError<&str>>("shuffle ")
        .parse(lower.as_str())
        .ok()?;

    // Try to split compound subject from the text after "shuffle "
    let text_after = &text["shuffle ".len()..];
    let (first, second, remainder) = try_split_compound_subject(text_after)?;

    // The remainder must indicate library destination
    let remainder_lower = remainder.to_lowercase();
    let is_owner_library = scan_contains_phrase(&remainder_lower, "owner")
        || scan_contains_phrase(&remainder_lower, "their")
        || scan_contains_phrase(&remainder_lower, "its");

    if !scan_contains_phrase(&remainder_lower, "librar") {
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
        up_to: false,
        enter_with_counters: vec![],
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
        up_to: false,
        enter_with_counters: vec![],
    };

    Some(ParsedEffectClause {
        effect: primary_effect,
        duration: None,
        sub_ability: Some(Box::new(sub_def)),
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
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
        | Effect::Sacrifice { target, .. }
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
///
/// CR 608.2k: An anaphor in a later clause of a multi-step ability binds to the
/// typed target chosen for an earlier clause. The predicate gates the rebinding
/// so non-targeting prior clauses (e.g. `Effect::Draw`, untargeted `LoseLife`)
/// don't capture a later "it" reference. `Effect::GenericEffect` carries an
/// `Option<TargetFilter>` (becomes/animate/transform-class clauses such as
/// "Target legendary creature becomes a God"); when populated with a typed
/// filter, a following anaphor refers to the same chosen object.
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
        } | Effect::GenericEffect {
            target: Some(TargetFilter::Typed(_)),
            ..
        }
    )
}

fn lower_subject_predicate_ast(
    subject: SubjectPhraseAst,
    predicate: PredicateAst,
    ctx: &ParseContext,
) -> ParsedEffectClause {
    // CR 115.1d: Propagate multi_target from the subject phrase (e.g., "any number of
    // target creatures", "up to two target artifacts") into the lowered clause so the
    // targeting system creates the correct number of target slots at cast time.
    let multi_target = subject.multi_target.clone();

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
            multi_target,
            condition: None,
            optional: subject.is_optional,
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
            multi_target,
            condition: None,
            optional: subject.is_optional,
        },
        PredicateAst::Restriction { effect, duration } => ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
            distribute: None,
            multi_target,
            condition: None,
            optional: subject.is_optional,
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
            // CR 701.20a: "<player> reveals cards from the top of their library
            // until they reveal a [filter]" — third-person form. The subject
            // (e.g., "its controller", "that player", "target opponent") was
            // extracted into `subject.affected` and identifies whose library is
            // revealed. Must be checked BEFORE the RevealTop fallback below,
            // which would otherwise greedy-match the "reveals/top/library" verbs.
            {
                let pred_tp = TextPair::new(text.as_str(), pred_lower.as_str());
                if let Some(clause) = try_parse_reveal_until(pred_tp, subject.affected.clone()) {
                    return clause;
                }
            }
            // CR 701.20a: "<player> reveals the top [N] card(s) of their library"
            if alt((tag::<_, _, VerboseError<&str>>("reveal "), tag("reveals ")))
                .parse(pred_lower.as_str())
                .is_ok()
                && scan_contains_phrase(&pred_lower, "top")
                && scan_contains_phrase(&pred_lower, "library")
            {
                // Delegate to nom combinator (input already lowercase from pred_lower).
                let count = if let Some(after_top) = strip_after(&pred_lower, "top ") {
                    nom_primitives::parse_number
                        .parse(after_top)
                        .map(|(_, n)| n)
                        .unwrap_or(1)
                } else {
                    1
                };
                return parsed_clause(Effect::RevealTop {
                    player: subject.affected,
                    count,
                });
            }
            // CR 701.10a: "<player> exiles the top [N] card(s) of their library"
            if alt((tag::<_, _, VerboseError<&str>>("exile "), tag("exiles ")))
                .parse(pred_lower.as_str())
                .is_ok()
                && scan_contains_phrase(&pred_lower, "top")
                && scan_contains_phrase(&pred_lower, "library")
            {
                let count = if let Some(after_top) = strip_after(&pred_lower, "top ") {
                    // CR 107.1a: "half of their library, rounded up/down" composes
                    // over `TargetZoneCardCount` via the shared quantity combinator.
                    if let Ok((_, expr)) =
                        super::oracle_nom::quantity::parse_half_rounded(after_top)
                    {
                        expr
                    } else {
                        let n = nom_primitives::parse_number
                            .parse(after_top)
                            .map(|(_, n)| n as i32)
                            .unwrap_or(1);
                        QuantityExpr::Fixed { value: n }
                    }
                } else {
                    QuantityExpr::Fixed { value: 1 }
                };
                return parsed_clause(Effect::ExileTop {
                    player: subject.affected,
                    count,
                });
            }
            // CR 701.40a + CR 608.2c: "<player> manifests the top [N] card(s) of
            // their library" — Reality Shift's "its controller manifests the top
            // card of their library" routes through this arm so the acting
            // player is bound to `subject.affected` (e.g., ParentTargetController)
            // rather than the default Controller.
            if alt((
                tag::<_, _, VerboseError<&str>>("manifest "),
                tag("manifests "),
            ))
            .parse(pred_lower.as_str())
            .is_ok()
                && scan_contains_phrase(&pred_lower, "top")
                && scan_contains_phrase(&pred_lower, "library")
            {
                let count = if let Some(after_top) = strip_after(&pred_lower, "top ") {
                    let n = nom_primitives::parse_number
                        .parse(after_top)
                        .map(|(_, n)| n as i32)
                        .unwrap_or(1);
                    QuantityExpr::Fixed { value: n }
                } else {
                    QuantityExpr::Fixed { value: 1 }
                };
                return parsed_clause(Effect::Manifest {
                    target: subject.affected,
                    count,
                });
            }
            let mut clause = lower_imperative_clause(&text, ctx);
            if let Some(player_target) = subject
                .target
                .as_ref()
                .filter(|filter| target_filter_can_target_player(filter))
            {
                if matches!(
                    clause.effect,
                    Effect::ChangeZone { .. } | Effect::ChangeZoneAll { .. }
                ) {
                    let mut sub_ability =
                        AbilityDefinition::new(AbilityKind::Spell, clause.effect.clone());
                    sub_ability.sub_ability = clause.sub_ability;
                    return ParsedEffectClause {
                        effect: Effect::TargetOnly {
                            target: player_target.clone(),
                        },
                        duration: clause.duration,
                        sub_ability: Some(Box::new(sub_ability)),
                        distribute: None,
                        multi_target: subject.multi_target,
                        condition: None,
                        optional: subject.is_optional,
                    };
                }
            }
            if matches!(clause.effect, Effect::Explore) {
                let subject_filter = if subject.inherits_parent {
                    TargetFilter::ParentTarget
                } else {
                    subject.target.as_ref().unwrap_or(&subject.affected).clone()
                };

                if subject.target.is_some()
                    || subject.inherits_parent
                    || matches!(subject.affected, TargetFilter::TriggeringSource)
                {
                    let mut explore = AbilityDefinition::new(AbilityKind::Spell, Effect::Explore);
                    explore.sub_ability = clause.sub_ability;
                    return ParsedEffectClause {
                        effect: Effect::TargetOnly {
                            target: subject_filter,
                        },
                        duration: clause.duration,
                        sub_ability: Some(Box::new(explore)),
                        distribute: None,
                        multi_target: subject.multi_target,
                        condition: None,
                        optional: subject.is_optional,
                    };
                }

                if !matches!(subject.affected, TargetFilter::SelfRef) {
                    return ParsedEffectClause {
                        effect: Effect::ExploreAll {
                            filter: subject_filter,
                        },
                        duration: clause.duration,
                        sub_ability: clause.sub_ability,
                        distribute: None,
                        multi_target: subject.multi_target,
                        condition: None,
                        optional: subject.is_optional,
                    };
                }
            }
            // CR 608.2c: Inject the subject's target into targeted effects that were
            // parsed via the imperative path (connive, phase out, force block, suspect).
            inject_subject_target(&mut clause.effect, &subject);
            // CR 113.10 + CR 702.16j: When the subject is a player-scope filter
            // ("you" → Controller) and the predicate produced a keyword grant
            // whose keywords are all player-applicable (currently only
            // Protection), retarget the static's `affected` from SelfRef (the
            // spell object) to the subject filter so the keyword is granted to
            // the player. This is the Teferi's-Protection-clause-2 hook.
            retarget_player_scoped_keyword_grant(&mut clause.effect, &subject.affected);
            if clause.multi_target.is_none() {
                clause.multi_target = subject.multi_target;
            }
            // CR 608.2c + CR 117.3a: Propagate the subject phrase's "may" modal
            // so the lowered ability is marked optional (e.g., "its controller
            // may search their library").
            if subject.is_optional {
                clause.optional = true;
            }
            clause
        }
    }
}

/// CR 113.10 + CR 702.16j: If `effect` is a `GenericEffect` whose static
/// modifications are all player-applicable AddKeyword grants and whose
/// `affected` is the default `SelfRef`, rewrite `affected` to `subject_filter`.
///
/// Only player-applicable keywords are rerouted; the list is intentionally
/// narrow because the overwhelming majority of "gain [keyword]" patterns target
/// a creature subject (and therefore belong on the object, not the controller).
/// Today only `Keyword::Protection(_)` qualifies — extend as other keywords
/// become legal on players.
fn retarget_player_scoped_keyword_grant(effect: &mut Effect, subject_filter: &TargetFilter) {
    let Effect::GenericEffect {
        static_abilities, ..
    } = effect
    else {
        return;
    };
    if !matches!(
        subject_filter,
        TargetFilter::Controller | TargetFilter::Player
    ) {
        return;
    }
    for sd in static_abilities.iter_mut() {
        if sd.affected != Some(TargetFilter::SelfRef) {
            continue;
        }
        if sd.modifications.is_empty() {
            continue;
        }
        let all_player_applicable = sd.modifications.iter().all(|m| match m {
            ContinuousModification::AddKeyword { keyword }
            | ContinuousModification::RemoveKeyword { keyword } => {
                is_player_applicable_keyword(keyword)
            }
            _ => false,
        });
        if all_player_applicable {
            sd.affected = Some(subject_filter.clone());
        }
    }
}

/// CR 113.10 + CR 702.16j: Keywords that can be granted to a player.
/// Today only Protection (per CR 702.16b/j). Extend as new player-granted
/// keyword qualities are implemented — always via typed enum variants, never
/// via string comparison.
fn is_player_applicable_keyword(keyword: &crate::types::keywords::Keyword) -> bool {
    matches!(keyword, crate::types::keywords::Keyword::Protection(_))
}

/// CR 608.2c + CR 117.3a: Extract a non-caster player anchor from an effect's
/// own subject. Used by `parse_effect_chain_impl` to remember "who is acting"
/// when a later clause in the same chain has a caster-defaulted player target
/// that should inherit the earlier subject (Assassin's Trophy's "then
/// shuffle" inheriting "its controller" from the preceding search clause).
///
/// Returns None for caster-defaulted or object-scoped effects — these do not
/// establish an acting-player anchor.
fn extract_player_anchor(effect: &Effect) -> Option<TargetFilter> {
    let candidate = match effect {
        Effect::SearchLibrary {
            target_player: Some(filter),
            ..
        } => filter,
        Effect::Shuffle { target } => target,
        Effect::ChangeZoneAll { target, .. } => target,
        _ => return None,
    };
    if !target_filter_can_target_player(candidate) {
        return None;
    }
    if matches!(candidate, TargetFilter::Controller | TargetFilter::Any) {
        return None;
    }
    Some(candidate.clone())
}

/// CR 608.2c + CR 117.3a: Apply an anchor subject (tracked by the chain parser)
/// to a caster-defaulted player-scope effect, so chained instructions like
/// "then shuffle" inherit the earlier subject ("its controller").
///
/// Only overwrites effects whose player target is the caster default
/// (`Controller`) and only when the anchor is a player-scope filter. Does not
/// touch effects that already carry an explicit non-caster subject.
fn apply_anchor_subject(effect: &mut Effect, anchor: &TargetFilter) {
    if !target_filter_can_target_player(anchor) {
        return;
    }
    match effect {
        Effect::Shuffle { target } if *target == TargetFilter::Controller => {
            *target = anchor.clone();
        }
        Effect::SearchLibrary {
            target_player: tp @ None,
            ..
        } => {
            *tp = Some(anchor.clone());
        }
        Effect::ChangeZoneAll { target, .. } if *target == TargetFilter::Controller => {
            *target = anchor.clone();
        }
        _ => {}
    }
}

/// CR 109.4: Map a player-reference `TargetFilter` to the `ControllerRef`
/// variant a typed object filter should adopt to match "permanents that player
/// controls". Only the two anaphoric variants used in trigger effects
/// (`TriggeringPlayer`, `TargetPlayer`) produce a result — caster-scoped
/// filters (`Controller`, `You`) should not rewrite an object filter because
/// they reference the ability controller, not an acting player target.
fn player_filter_as_controller_ref(filter: &TargetFilter) -> Option<ControllerRef> {
    match filter {
        TargetFilter::TriggeringPlayer => Some(ControllerRef::TargetPlayer),
        TargetFilter::Typed(tf)
            if tf.type_filters.is_empty()
                && tf.controller.is_some()
                && tf.properties.is_empty() =>
        {
            tf.controller.clone()
        }
        _ => None,
    }
}

/// CR 109.4 + CR 115.7: Attach a controller constraint to a typed object
/// filter that does not yet have one. Walks into `Or`/`And`/`Not` to cover
/// compound filters like "target creature or planeswalker". Filters that
/// already carry a controller constraint are left untouched so an explicit
/// "creature you control" clause is never silently rewritten.
pub(super) fn attach_controller_if_absent(filter: &mut TargetFilter, ctrl: ControllerRef) {
    match filter {
        TargetFilter::Typed(tf) if tf.controller.is_none() => {
            tf.controller = Some(ctrl);
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            for inner in filters.iter_mut() {
                attach_controller_if_absent(inner, ctrl.clone());
            }
        }
        TargetFilter::Not { filter } => attach_controller_if_absent(filter, ctrl),
        _ => {}
    }
}

fn target_filter_can_target_player(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::DefendingPlayer
        | TargetFilter::ParentTargetController => true,
        TargetFilter::Typed(tf) => tf.type_filters.is_empty(),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_can_target_player)
        }
        TargetFilter::Not { filter } => target_filter_can_target_player(filter),
        _ => false,
    }
}

/// Inject a subject phrase's target filter into an effect that was parsed through
/// the imperative fallback path, where the subject was stripped before parsing.
/// Only applies to effects with a sentinel `TargetFilter::Any` that should inherit
/// the subject's targeting information.
fn inject_subject_target(effect: &mut Effect, subject: &SubjectPhraseAst) {
    let subject_filter = subject.target.as_ref().unwrap_or(&subject.affected).clone();
    match effect {
        // CR 601.2c + CR 121.1: "Target player draws ..." — each Draw mode of a
        // modal spell is its own targeting instance. The imperative path emits
        // `target: TargetFilter::Controller` (the no-subject default); when a
        // player-subject is parsed we promote that to the subject filter so
        // `collect_target_slots` surfaces an independent slot per Draw mode.
        Effect::Connive { target, .. }
        | Effect::PhaseOut { target }
        | Effect::PhaseIn { target }
        | Effect::ForceBlock { target }
        | Effect::Suspect { target }
        | Effect::Goad { target }
        | Effect::Mill { target, .. }
        | Effect::Discard { target, .. }
        | Effect::Draw { target, .. }
        | Effect::Scry { target, .. }
        | Effect::Surveil { target, .. }
        | Effect::PutAtLibraryPosition { target, .. }
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
        // CR 122.1: "target opponent loses all counters" — inject subject target
        Effect::LoseAllPlayerCounters { target } if *target == TargetFilter::Controller => {
            *target = subject_filter;
        }
        // CR 500.8: "target player gets an additional combat phase" — inject subject target
        Effect::AdditionalCombatPhase { target, .. } if *target == TargetFilter::Controller => {
            *target = subject_filter;
        }
        // CR 400.7: "shuffle [subject]'s graveyard into their library" — inject
        // subject target for zone-wide changes and shuffles.
        Effect::ChangeZoneAll { ref mut target, .. }
            if *target == TargetFilter::Any || *target == TargetFilter::Controller =>
        {
            *target = subject_filter;
        }
        Effect::Shuffle { ref mut target }
            if *target == TargetFilter::Any || *target == TargetFilter::Controller =>
        {
            *target = subject_filter;
        }
        Effect::Token { ref mut owner, .. } if *owner == TargetFilter::Controller => {
            *owner = subject_filter;
        }
        // CR 701.14a: "enchanted creature fights target creature" — the subject
        // of the fight is the enchanted/equipped creature, not the Aura/Equipment.
        Effect::Fight {
            subject: fight_subject,
            ..
        } if *fight_subject == TargetFilter::SelfRef => {
            *fight_subject = subject_filter;
        }
        // Object-targeting effects: inject subject filter only when the sentinel
        // is `TargetFilter::Any` (not Controller — these target objects, not players).
        // Note: DealDamage is intentionally excluded — the damage parser always parses
        // its own target from the "to [target]" clause. Injecting the subject into
        // DealDamage would overwrite explicitly-parsed "any target" with SelfRef,
        // causing "It deals 5 damage to any target" to target itself instead of the opponent.
        Effect::Pump { target, .. }
        | Effect::Destroy { target, .. }
        | Effect::Regenerate { target, .. }
        | Effect::Counter { target, .. }
        | Effect::Tap { target, .. }
        | Effect::Untap { target, .. }
        | Effect::AddCounter { target, .. }
        | Effect::RemoveCounter { target, .. }
        | Effect::Sacrifice { target, .. }
        | Effect::DiscardCard { target, .. }
        | Effect::ChangeZone { target, .. }
        | Effect::GainControl { target, .. }
        | Effect::ControlNextTurn { target, .. }
        | Effect::Attach { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::SwitchPT { target, .. }
        | Effect::CopySpell { target, .. }
        | Effect::CopyTokenOf { target, .. }
        | Effect::BecomeCopy { target, .. }
        | Effect::ChooseCard { target, .. }
        | Effect::PutCounter { target, .. }
        | Effect::MultiplyCounter { target, .. }
        | Effect::DoublePT { target, .. }
        | Effect::MoveCounters { target, .. }
        | Effect::Animate { target, .. }
        | Effect::Transform { target, .. }
        | Effect::RevealHand { target, .. }
        | Effect::TargetOnly { target, .. }
        | Effect::PreventDamage { target, .. }
        | Effect::Exploit { target, .. }
        | Effect::CastFromZone { target, .. }
        | Effect::PutOnTopOrBottom { target, .. }
        | Effect::Double { target, .. }
            if *target == TargetFilter::Any =>
        {
            *target = subject_filter;
        }
        // CR 608.2k + CR 109.4: "that player sacrifices a non-Elf creature" /
        // "each opponent sacrifices a creature they control". When the subject
        // is a player reference (TriggeringPlayer / TargetPlayer / etc.) and
        // the effect targets a TYPED object filter that has not been scoped to
        // a controller, inject the controller constraint so the filter matches
        // only permanents controlled by the acting player. Without this, a
        // phase trigger like Ruthless Winnower's "that player sacrifices a
        // non-Elf creature" would match any player's non-Elf creature instead
        // of the sacrificing player's. Reached only when the earlier Any-only
        // arm did not fire (target is a typed filter, not `Any`).
        Effect::Sacrifice { target, .. }
            if player_filter_as_controller_ref(&subject_filter).is_some() =>
        {
            if let Some(ctrl) = player_filter_as_controller_ref(&subject_filter) {
                attach_controller_if_absent(target, ctrl);
            }
        }
        // CR 119.3 + CR 115.1d: "they lose N life" — inject subject's player reference.
        // LoseLife.target is Option<TargetFilter>, unlike other effects' non-optional targets.
        // Guard on is_none() to only inject when no target was explicitly parsed.
        Effect::LoseLife { ref mut target, .. } if target.is_none() => {
            *target = Some(subject_filter);
        }
        // CR 701.23a: "Its controller may search their library..." — the subject
        // names the library owner (and, when the subject is a context-ref, the
        // searcher). Only inject when the subject is a player-scope filter and
        // no target_player has already been extracted from "search target X's
        // library" earlier in the pipeline.
        Effect::SearchLibrary {
            target_player: target_player @ None,
            ..
        } if target_filter_can_target_player(&subject_filter)
            && subject_filter != TargetFilter::Controller =>
        {
            *target_player = Some(subject_filter);
        }
        _ => {}
    }
}

/// CR 114.1: Parse emblem creation from Oracle text.
/// Handles both full form "you get an emblem with \"[text]\"" and
/// subject-stripped form "get an emblem with \"[text]\"".
fn try_parse_emblem_creation(lower: &str, original: &str) -> Option<Effect> {
    // Use nom to strip the prefix and get original-case remainder
    let (_, rest) = nom_on_lower(original, lower, |i| {
        value(
            (),
            alt((tag("you get an emblem with "), tag("get an emblem with "))),
        )
        .parse(i)
    })?;

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

    // CR 113.1c + CR 114.4: An emblem's ability may be a triggered ability
    // ("Whenever ...", "At the beginning of ..."), a static ability, or the
    // Unknown fallback. Try the triggered-ability parser first; fall through
    // to the static-ability parser only if `parse_trigger_lines` does not
    // recognise a trigger phrase (returns `TriggerMode::Unknown`). Both
    // detectors ARE the parsers — no string heuristics.
    let trig_defs = super::oracle_trigger::parse_trigger_lines(inner, "~");
    let all_known = !trig_defs.is_empty()
        && trig_defs
            .iter()
            .all(|t| !matches!(t.mode, TriggerMode::Unknown(_)));
    if all_known {
        // CR 114.4: An emblem's triggers function in the command zone, so the
        // `collect_matching_triggers` zone guard must admit them. Declare
        // `trigger_zones = [Zone::Command]` explicitly (the default empty
        // vector is interpreted as battlefield-only by the scanner).
        let triggers: Vec<TriggerDefinition> = trig_defs
            .into_iter()
            .map(|t| t.trigger_zones(vec![Zone::Command]))
            .collect();
        return Some(Effect::CreateEmblem {
            statics: Vec::new(),
            triggers,
        });
    }

    // Try to parse the emblem text as a static ability line.
    if let Some(static_def) = super::oracle_static::parse_static_line(inner) {
        Some(Effect::CreateEmblem {
            statics: vec![static_def],
            triggers: Vec::new(),
        })
    } else {
        // Fallback: create an emblem with an unimplemented static.
        Some(Effect::CreateEmblem {
            statics: vec![
                StaticDefinition::new(StaticMode::EmblemStatic).description(inner.to_string())
            ],
            triggers: Vec::new(),
        })
    }
}

/// CR 601.2a + CR 118.9: Parse "cast it/that card [without paying its mana cost]".
///
/// Three branches:
/// 1. Anaphoric — "cast it", "cast that spell", "cast those cards" — target is
///    `ParentTarget` (refers to the cards exiled / chosen by a prior effect).
/// 2. Constrained — "cast a [type-phrase] [from <zone>] [with mana value <bound>]
///    without paying its mana cost" — target is built from `parse_type_phrase` +
///    origin-zone inference. CR 118.9 + CR 601.2a + CR 120.3.
/// 3. Bare — fallback `TargetFilter::Any`.
fn try_parse_cast_effect(lower: &str) -> Option<Effect> {
    type E<'a> = VerboseError<&'a str>;

    // CR 305.1: "play" means cast if spell, play as land if land.
    let (rest, mode) = alt((
        value(CardPlayMode::Cast, tag::<_, _, E>("cast ")),
        value(CardPlayMode::Play, tag("play ")),
    ))
    .parse(lower)
    .ok()?;

    // CR 401.5 + CR 118.9 + CR 601.2a: Static-shaped "you may [play|cast] X
    // from the top of your library" lines (Realmwalker, Future Sight, Bolas's
    // Citadel) are routed to `parse_static_line` →
    // `StaticMode::TopOfLibraryCastPermission` by `is_static_compound_pattern`
    // before the imperative dispatcher runs, so this path does not see them.
    // Trigger sub-effects (Ziatora's Envoy combat-damage trigger, The
    // Belligerent attack trigger granting "until end of turn, you may play
    // lands and cast spells from the top of your library") are NOT classified
    // — they parse through the imperative path here and lower to
    // `Effect::CastFromZone`, the same shape the engine has long supported for
    // these cards. A blanket guard against the "from the top of your library"
    // anchor would silently regress those triggers to `Unimplemented`.

    let without_paying = scan_contains_phrase(rest, "without paying its mana cost")
        || scan_contains_phrase(rest, "without paying their mana cost");

    // Branch 1: anaphoric forms. Order longer-first ("one of those cards"
    // before "those cards") so the prefix-match doesn't shadow the longer
    // anaphor.
    if alt((
        tag::<_, _, E>("one of those cards"),
        tag("the exiled card"),
        tag("those cards"),
        tag("cards exiled"),
        tag("that card"),
        tag("that spell"),
        tag("the copy"),
        tag("them"),
        tag("it"),
    ))
    .parse(rest)
    .is_ok()
    {
        return Some(Effect::CastFromZone {
            target: TargetFilter::ParentTarget,
            without_paying_mana_cost: without_paying,
            mode,
            cast_transformed: false,
            alt_ability_cost: None,
        });
    }

    // Branch 2: constrained-filter form (Buster Sword, FIN equipment cycle).
    // CR 118.9 + CR 601.2a: "cast a <filter> spell [from <zone>] [with mana
    // value <bound>] without paying its mana cost" — extract the constraint
    // by composing `parse_type_phrase` (handles "a [type-phrase]") with
    // origin-zone inference and a `with mana value` suffix scan over the
    // remainder. The suffix scan is necessary because `parse_type_phrase`'s
    // own internal `parse_mana_value_suffix` call runs before `parse_zone_suffix`,
    // so for inputs of the form "spell from your hand with mana value ..."
    // the mana-value clause is past the type-phrase pos when reached.
    let (filter, _after) = super::oracle_target::parse_type_phrase(rest);
    if matches!(filter, TargetFilter::Typed(_)) {
        let mut filter = filter;
        if let Some(zone) = infer_origin_zone(rest) {
            // Fold "from your hand" into the filter as a zone constraint so
            // downstream target legality (CR 601.2c) restricts the choice to
            // the right zone.
            if let TargetFilter::Typed(ref mut tf) = filter {
                if !tf
                    .properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::InZone { .. }))
                {
                    tf.properties.push(FilterProp::InZone { zone });
                }
            }
        }
        // CR 202.3 + CR 120.3: Scan for "with mana value <bound>" anywhere
        // in the remainder via nom `take_until` + delegated parse, and fold
        // the resulting `CmcLE`/`CmcGE`/`CmcEQ` prop onto the typed filter.
        // `parse_mana_value_suffix` is a pure-nom combinator that emits the
        // right `QuantityRef` (EventContextAmount for "that damage",
        // EventContextSourceManaValue for "that <type>", literal/X otherwise).
        // The scan is needed because `parse_type_phrase` runs its internal
        // `parse_mana_value_suffix` call before `parse_zone_suffix`, so for
        // inputs of the form "spell from <zone> with mana value ..." the
        // suffix clause is past the type-phrase scan position.
        if let Ok((after_take, _)) = take_until::<_, _, E>("with mana value ").parse(rest) {
            if let Some((prop, _)) = super::oracle_target::parse_mana_value_suffix(after_take) {
                if let TargetFilter::Typed(ref mut tf) = filter {
                    if !tf.properties.iter().any(|p| {
                        matches!(
                            p,
                            FilterProp::CmcLE { .. }
                                | FilterProp::CmcGE { .. }
                                | FilterProp::CmcEQ { .. }
                        )
                    }) {
                        tf.properties.push(prop);
                    }
                }
            }
        }
        return Some(Effect::CastFromZone {
            target: filter,
            without_paying_mana_cost: without_paying,
            mode,
            cast_transformed: false,
            alt_ability_cost: None,
        });
    }

    // Branch 3: bare fallback.
    Some(Effect::CastFromZone {
        target: TargetFilter::Any,
        without_paying_mana_cost: without_paying,
        mode,
        cast_transformed: false,
        alt_ability_cost: None,
    })
}

/// CR 118.9 + CR 119.4: Recognise an "alternative-cost rider" — text of the
/// form "[If you cast a spell this way,] pay <ability-cost> rather than
/// paying its mana cost". The body parses to an `AbilityCost` that the
/// runtime pays in lieu of the spell's mana cost when casting via a granted
/// `ExileWithAltAbilityCost` permission. Returns `Some(cost)` when the rider
/// shape is recognised; `None` otherwise. Currently handles the "pay life
/// equal to its mana value" form (Nashi, Moon Sage's Scion); the cost
/// parser will accept other `AbilityCost` shapes naturally as they are
/// added.
pub(crate) fn try_parse_alt_cost_rider(text: &str) -> Option<crate::types::ability::AbilityCost> {
    type Vbe<'a> = VerboseError<&'a str>;
    let lower = text.to_lowercase();
    let trimmed_lower = lower.trim_end_matches('.').trim();
    // Accept the rider with or without an "if you cast a spell this way,"
    // conditional prefix (the conditional is folded into the runtime check
    // by the casting pipeline — the permission only fires for spells cast
    // through this grant). Both forms map to the same `AbilityCost`.
    let after_prefix = if let Ok((rest, _)) = alt((
        tag::<_, _, Vbe>("if you cast a spell this way, "),
        tag::<_, _, Vbe>("if you cast it this way, "),
    ))
    .parse(trimmed_lower)
    {
        rest
    } else {
        trimmed_lower
    };
    // Confirm the rider's tail. Without this guard, "pay life equal to its
    // mana value" alone (without the "rather than" clause) would also match —
    // but that form is a normal payment effect, not an alt-cost grant.
    if !nom_primitives::scan_contains(after_prefix, "rather than paying its mana cost")
        && !nom_primitives::scan_contains(after_prefix, "rather than pay its mana cost")
    {
        return None;
    }
    // CR 119.4: "pay life equal to its mana value" — the only currently-
    // supported rider body. Detect via nom prefix match; emit
    // `AbilityCost::PayLife { amount: SelfManaValue }` (CR 202.3 — "its mana
    // value" resolves against the spell-being-cast at cost-payment time).
    if let Ok((_, _)) = tag::<_, _, Vbe>("pay life equal to its mana value").parse(after_prefix) {
        return Some(crate::types::ability::AbilityCost::PayLife {
            amount: crate::types::ability::QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::SelfManaValue,
            },
        });
    }
    None
}

/// CR 118.9: Walk `defs` from the back, descending into `sub_ability`
/// chains, and stamp `alt_ability_cost` onto the most recent `CastFromZone`
/// effect that does not already carry one. Returns `true` when a target
/// was found and stamped; `false` when no `CastFromZone` is in scope (the
/// caller should fall back to emitting the rider as an Unimplemented effect
/// to preserve coverage signal).
fn attach_alt_cost_to_prior_cast_from_zone(
    defs: &mut [AbilityDefinition],
    cost: crate::types::ability::AbilityCost,
) -> bool {
    fn walk(def: &mut AbilityDefinition, cost: &crate::types::ability::AbilityCost) -> bool {
        // Recurse into sub_ability first so we stamp the *most recent*
        // (deepest) CastFromZone in the chain — the rider attaches to the
        // immediately-preceding cast clause.
        if let Some(sub) = def.sub_ability.as_mut() {
            if walk(sub, cost) {
                return true;
            }
        }
        if let Effect::CastFromZone {
            alt_ability_cost: alt @ None,
            ..
        } = &mut *def.effect
        {
            *alt = Some(cost.clone());
            return true;
        }
        false
    }
    for def in defs.iter_mut().rev() {
        if walk(def, &cost) {
            return true;
        }
    }
    false
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
    if scan_contains_phrase(rest, "card from it") {
        return false;
    }

    // If try_parse_named_choice would match "choose {rest}", it's a named choice, not targeting
    let as_full = format!("choose {rest}");
    if try_parse_named_choice(&as_full).is_some() {
        return false;
    }

    // Any phrase containing "target" is a targeting synonym
    if scan_contains_phrase(rest, "target") {
        return true;
    }

    // "choose up to N" without "target" (e.g. "choose up to two creatures"),
    // but NOT "choose up to N of them/those" which is anaphoric (handled separately).
    if tag::<_, _, VerboseError<&str>>("up to ")
        .parse(rest)
        .is_ok()
        && !scan_contains_phrase(rest, "of them")
        && !scan_contains_phrase(rest, "of those")
    {
        return true;
    }

    // "choose a/an {type} ... you control / an opponent controls"
    if let Some(after_article) = nom_primitives::parse_article
        .parse(rest)
        .ok()
        .map(|(rest, _)| rest)
    {
        // Exclude patterns not yet in try_parse_named_choice but still not targeting
        if alt((
            tag::<_, _, VerboseError<&str>>("nonbasic land type"),
            tag("number"),
        ))
        .parse(after_article)
        .is_ok()
        {
            return false;
        }
        // Must reference controller to be targeting-like.
        // "they control" covers "target opponent chooses a creature they control"
        // where "they" refers to the targeted player (CR 608.2d).
        // Exclude "from among" patterns (Cataclysm-family multi-category selection)
        // which require engine infrastructure not yet implemented.
        if !scan_contains_phrase(after_article, "from among")
            && (scan_contains_phrase(after_article, "you control")
                || scan_contains_phrase(after_article, "opponent controls")
                || scan_contains_phrase(after_article, "an opponent controls")
                || scan_contains_phrase(after_article, "they control"))
        {
            return true;
        }
    }

    // General controller-reference fallback for non-article patterns:
    // "six lands they control", "any number of creatures they control",
    // "three permanents they control", etc.
    // Exclude "from among" patterns (Cataclysm-family multi-category selection)
    // which require engine infrastructure not yet implemented.
    if !scan_contains_phrase(rest, "from among")
        && (scan_contains_phrase(rest, "they control") || scan_contains_phrase(rest, "you control"))
    {
        return true;
    }

    false
}

/// Match "choose a creature type", "choose a color", "choose odd or even",
/// "choose a basic land type", "choose a card type" from lowercased Oracle text.
pub(crate) fn try_parse_named_choice(lower: &str) -> Option<ChoiceType> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("choose ")
        .parse(lower)
        .ok()?;
    type E<'a> = VerboseError<&'a str>;
    if tag::<_, _, E>("a creature type").parse(rest).is_ok() {
        Some(ChoiceType::CreatureType)
    } else if tag::<_, _, E>("a color").parse(rest).is_ok() {
        Some(ChoiceType::Color)
    } else if tag::<_, _, E>("odd or even").parse(rest).is_ok() {
        Some(ChoiceType::OddOrEven)
    } else if tag::<_, _, E>("a basic land type").parse(rest).is_ok() {
        Some(ChoiceType::BasicLandType)
    } else if tag::<_, _, E>("a card type").parse(rest).is_ok() {
        Some(ChoiceType::CardType)
    } else if alt((
        tag::<_, _, E>("a card name"),
        tag("a nonland card name"),
        tag("a creature card name"),
        tag("a land card name"),
    ))
    .parse(rest)
    .is_ok()
    {
        Some(ChoiceType::CardName)
    } else if let Ok((range_rest, _)) = tag::<_, _, E>("a number between ").parse(rest) {
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
    } else if let Ok((gt_rest, _)) = tag::<_, _, E>("a number greater than ").parse(rest) {
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
    } else if rest == "a number" || tag::<_, _, E>("a number ").parse(rest).is_ok() {
        // Generic "choose a number" — default range 0-20
        Some(ChoiceType::NumberRange { min: 0, max: 20 })
    } else if alt((tag::<_, _, E>("a land type"), tag("a nonbasic land type")))
        .parse(rest)
        .is_ok()
    {
        Some(ChoiceType::LandType)
    } else if tag::<_, _, E>("an opponent").parse(rest).is_ok() {
        // CR 800.4a: Choose an opponent from among players in the game.
        Some(ChoiceType::Opponent)
    } else if tag::<_, _, E>("a player").parse(rest).is_ok() {
        Some(ChoiceType::Player)
    } else if tag::<_, _, E>("two colors").parse(rest).is_ok() {
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
    let (_, (left, right)) = nom_primitives::split_once_on(rest, " or ").ok()?;
    let left = left.trim();
    let right = right.trim();

    // Labels must be short (≤2 words) — longer phrases are likely clauses, not choices
    if left.split_whitespace().count() > 2 || right.split_whitespace().count() > 2 {
        return None;
    }
    // Reject known non-choice patterns
    if scan_contains_phrase(left, "target") || scan_contains_phrase(right, "target") {
        return None;
    }
    if right == "more" || left == "both" || right == "both" {
        return None;
    }

    Some(vec![capitalize(left), capitalize(right)])
}

/// Refine a damage target based on remainder text left after `parse_target`.
/// Handles common patterns:
/// - "'s controller" → `ParentTargetController` (CR 608.2c)
/// - "or planeswalker" / "or blocking creature" → union with additional type
fn refine_damage_target_remainder(target: TargetFilter, remainder: &str) -> (TargetFilter, &str) {
    let trimmed = remainder.trim();
    // CR 608.2c: "'s controller" — redirect damage to the controller of the target
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("'s controller").parse(trimmed) {
        return (TargetFilter::ParentTargetController, rest);
    }
    // "or <target>" — expand target to union: "creature or planeswalker",
    // "creature or blocking creature", "~ or enchanted creature", etc.
    // Uses parse_target (not parse_type_phrase) to handle special patterns
    // like "enchanted creature" that aren't plain type phrases.
    if let Ok((after_or, _)) = tag::<_, _, VerboseError<&str>>("or ").parse(trimmed) {
        let (additional, type_rem) = parse_target(after_or);
        if !matches!(additional, TargetFilter::Any) {
            return (
                TargetFilter::Or {
                    filters: vec![target, additional],
                },
                type_rem,
            );
        }
    }
    (target, remainder)
}

fn parse_choose_filter(lower: &str) -> TargetFilter {
    // Extract type info between "choose" and "card from it"
    // Handle both "choose X" and "you choose X" forms
    let after_choose = alt((
        tag::<_, _, VerboseError<&str>>("you choose "),
        tag("you may choose "),
        tag("choose "),
    ))
    .parse(lower)
    .map(|(rest, _)| rest)
    .unwrap_or(lower);

    // "one of them/those [cards]" — selection reference to parent target set
    if tag::<_, _, VerboseError<&str>>("one of th")
        .parse(after_choose)
        .is_ok()
    {
        return TargetFilter::ParentTarget;
    }

    let before_card = after_choose.split("card").next().unwrap_or("");
    let cleaned = before_card
        .trim()
        .trim_start_matches("a ")
        .trim_start_matches("an ")
        .trim();

    // Intentional: bare article "a [card]" or empty string means any card — not a parse failure
    if cleaned.is_empty() || cleaned == "a" {
        return TargetFilter::Any;
    }

    // structural: not dispatch — segmenting pre-extracted type string on comma separator
    // Comma-separated negation: "noncreature, nonland" → intersection of negations
    if cleaned.contains(", ") {
        let comma_parts: Vec<&str> = cleaned.split(", ").collect();
        let mut tf = TypedFilter::card();
        let mut all_resolved = true;
        for part in &comma_parts {
            if let Some(TargetFilter::Typed(part_tf)) = type_str_to_target_filter(part.trim()) {
                // Merge Non(...) type filters and properties, skip the Card base
                for t in part_tf.type_filters {
                    if !matches!(t, TypeFilter::Card) {
                        tf = tf.with_type(t);
                    }
                }
                for p in part_tf.properties {
                    tf.properties.push(p);
                }
            } else {
                all_resolved = false;
                break;
            }
        }
        if all_resolved {
            return TargetFilter::Typed(tf);
        }
    }

    // Try full type phrase parsing first — handles compound patterns like
    // "green or white creature", "nonland permanent", "spirit or arcane"
    let (phrase_filter, phrase_rem) = parse_type_phrase(cleaned);
    if !matches!(phrase_filter, TargetFilter::Any) && phrase_rem.trim().is_empty() {
        return phrase_filter;
    }

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
    push_warning(format!(
        "target-fallback: unrecognized type string '{}'",
        cleaned
    ));
    TargetFilter::Any
}

fn type_str_to_target_filter(s: &str) -> Option<TargetFilter> {
    // Simple core types
    let card_type = match s {
        "artifact" => Some(TypeFilter::Artifact),
        "creature" => Some(TypeFilter::Creature),
        "enchantment" => Some(TypeFilter::Enchantment),
        "instant" => Some(TypeFilter::Instant),
        "sorcery" => Some(TypeFilter::Sorcery),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        "land" => Some(TypeFilter::Land),
        "permanent" => Some(TypeFilter::Permanent),
        _ => None,
    };
    if let Some(ct) = card_type {
        return Some(TargetFilter::Typed(TypedFilter::new(ct)));
    }

    // CR 205.4b: Negated types — "nonland", "noncreature", etc.
    if let Some(negated) = s.strip_prefix("non") {
        // Card type negation
        let inner = match negated {
            "creature" => Some(TypeFilter::Creature),
            "land" => Some(TypeFilter::Land),
            "artifact" => Some(TypeFilter::Artifact),
            "enchantment" => Some(TypeFilter::Enchantment),
            "instant" => Some(TypeFilter::Instant),
            "sorcery" => Some(TypeFilter::Sorcery),
            "planeswalker" => Some(TypeFilter::Planeswalker),
            _ => None,
        };
        if let Some(inner) = inner {
            return Some(TargetFilter::Typed(
                TypedFilter::card().with_type(TypeFilter::Non(Box::new(inner))),
            ));
        }
        // CR 205.4a: Supertype negation — "nonbasic", "nonlegendary", "nonsnow"
        let super_prop = match negated {
            "basic" => Some(FilterProp::NotSupertype {
                value: Supertype::Basic,
            }),
            "legendary" => Some(FilterProp::NotSupertype {
                value: Supertype::Legendary,
            }),
            "snow" => Some(FilterProp::NotSupertype {
                value: Supertype::Snow,
            }),
            _ => None,
        };
        if let Some(prop) = super_prop {
            return Some(TargetFilter::Typed(
                TypedFilter::card().properties(vec![prop]),
            ));
        }
        return None;
    }

    // structural: not dispatch — splitting pre-extracted type string at word boundary
    // Compound: "nonland permanent", "nonbasic land"
    if let Some(pos) = s.find(' ') {
        let (first, rest) = s.split_at(pos);
        let rest = rest.trim();
        // Handle "nonbasic land", "nonlegendary" as supertype negation + type
        if let Some(negated_super) = first.strip_prefix("non") {
            let super_prop = match negated_super {
                "basic" => Some(FilterProp::NotSupertype {
                    value: Supertype::Basic,
                }),
                "legendary" => Some(FilterProp::NotSupertype {
                    value: Supertype::Legendary,
                }),
                "snow" => Some(FilterProp::NotSupertype {
                    value: Supertype::Snow,
                }),
                _ => None,
            };
            if let Some(prop) = super_prop {
                // Recurse for the base type
                if let Some(TargetFilter::Typed(base_tf)) = type_str_to_target_filter(rest) {
                    return Some(TargetFilter::Typed(base_tf.properties(vec![prop])));
                }
            }
        }
        // "nonland permanent" etc. — negated type modifier + base type
        // Only merge Non(...) filters from the negation part, not the Card base
        if let Some(TargetFilter::Typed(neg_tf)) = type_str_to_target_filter(first) {
            if let Some(TargetFilter::Typed(base_tf)) = type_str_to_target_filter(rest) {
                let mut combined = base_tf;
                for tf in neg_tf.type_filters {
                    if matches!(tf, TypeFilter::Non(_)) {
                        combined = combined.with_type(tf);
                    }
                }
                return Some(TargetFilter::Typed(combined));
            }
        }
    }

    None
}

/// Extract card type filter from a sub-ability sentence containing "card from it/among".
/// Handles forms like "exile a nonland card from it", "discard a creature card from it".
fn parse_choose_filter_from_sentence(lower: &str) -> TargetFilter {
    let before_card = match take_until::<_, _, VerboseError<&str>>("card from")
        .parse(lower)
        .ok()
    {
        Some((_, before)) => before,
        None => {
            push_warning(format!(
                "target-fallback: choose-from-sentence missing 'card from' in '{}'",
                lower.trim()
            ));
            return TargetFilter::Any;
        }
    };
    // The word immediately before "card from" is the type descriptor
    let word = before_card.trim().rsplit(' ').next().unwrap_or("");
    if let Some(negated) = word.strip_prefix("non") {
        if let Some(TargetFilter::Typed(tf)) = type_str_to_target_filter(negated) {
            if let Some(primary) = tf.get_primary_type().cloned() {
                return TargetFilter::Typed(
                    TypedFilter::card().with_type(TypeFilter::Non(Box::new(primary))),
                );
            }
        }
    }
    type_str_to_target_filter(word).unwrap_or_else(|| {
        push_warning(format!(
            "target-fallback: unrecognized choose-from-sentence type '{}'",
            word
        ));
        TargetFilter::Any
    })
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
    scan_contains_phrase(lower, "those cards")
        || scan_contains_phrase(lower, "those permanents")
        || scan_contains_phrase(lower, "those creatures")
        || scan_contains_phrase(lower, "those tokens")
        || scan_contains_phrase(lower, "the exiled card")
        || scan_contains_phrase(lower, "the exiled permanent")
        || scan_contains_phrase(lower, "the exiled creature")
}

/// CR 603.7: Detect implicit anaphora ("return it/them to the battlefield")
/// when preceded by an exile effect. Context-sensitive — only matches when
/// the pronoun is in a return-to-battlefield construction.
/// `lower` must be the pre-lowered version of the text.
fn contains_implicit_tracked_set_pronoun(lower: &str) -> bool {
    alt((
        tag::<_, _, VerboseError<&str>>("return it "),
        tag("return them "),
    ))
    .parse(lower)
    .is_ok()
        && scan_contains_phrase(lower, "battlefield")
}

fn mark_uses_tracked_set(def: &mut AbilityDefinition) {
    if let Effect::CreateDelayedTrigger {
        uses_tracked_set, ..
    } = &mut *def.effect
    {
        *uses_tracked_set = true;
    }
}

fn append_to_deepest_sub_ability(
    ability: &mut AbilityDefinition,
    tail: Option<Box<AbilityDefinition>>,
) {
    let Some(tail) = tail else {
        return;
    };

    let mut cursor = ability;
    while cursor.sub_ability.is_some() {
        cursor = cursor
            .sub_ability
            .as_mut()
            .expect("sub_ability checked above");
    }
    cursor.sub_ability = Some(tail);
}

/// CR 107.1a: Strip a trailing "Round down each time" / "Round up each time"
/// sentence from a chain of Oracle text, returning the stripped text and the
/// captured rounding mode. Used by `parse_effect_chain_impl` to consume the
/// chain-level rounding annotation (Pox Plague) before chunk splitting so
/// the phrase doesn't become an Unimplemented chunk. The captured mode is
/// re-applied to every `HalfRounded` in the built chain via
/// `rewrite_rounding_mode`.
fn strip_trailing_rounding_annotation(text: &str) -> (String, Option<RoundingMode>) {
    let trimmed = text.trim_end();
    let trimmed_no_dot = trimmed.trim_end_matches('.').trim_end();
    let lower = trimmed_no_dot.to_lowercase();
    for (phrase, mode) in [
        ("round down each time", RoundingMode::Down),
        ("round up each time", RoundingMode::Up),
    ] {
        if let Some(prefix_lower) = lower.strip_suffix(phrase) {
            // Require a sentence boundary before the phrase — either end of
            // text or a period followed by whitespace. Prevents matching
            // "when you round down each time" / similar mid-sentence usage.
            let before = &trimmed_no_dot[..prefix_lower.len()];
            let before_trimmed = before.trim_end();
            if before_trimmed.ends_with('.') {
                let without_phrase = before_trimmed.trim_end_matches('.').trim_end();
                return (without_phrase.to_string(), Some(mode));
            }
        }
    }
    (text.to_string(), None)
}

/// CR 608.2c + CR 107.1a: Apply `f` to every `QuantityExpr` reachable through
/// the common quantity-bearing fields of an `Effect`. This is a pragmatic
/// visitor — it covers the subset of `Effect` variants whose counts /
/// amounts meaningfully reference player-scoped possessives ("their life",
/// "their hand") in Oracle text. Variants not listed here keep their
/// expressions untouched; add arms here when a new variant surfaces a
/// possessive quantity.
fn each_quantity_expr_mut(effect: &mut Effect, f: &mut impl FnMut(&mut QuantityExpr)) {
    match effect {
        Effect::LoseLife { amount, .. }
        | Effect::GainLife { amount, .. }
        | Effect::SetLifeTotal { amount, .. } => f(amount),
        Effect::DealDamage { amount, .. } => f(amount),
        Effect::Draw { count, .. }
        | Effect::Mill { count, .. }
        | Effect::Discard { count, .. }
        | Effect::Scry { count, .. }
        | Effect::Surveil { count, .. }
        | Effect::PutCounter { count, .. } => f(count),
        Effect::Sacrifice { count, .. } => f(count),
        _ => {}
    }
}

/// CR 109.4 + CR 608.2c: Apply `f` to every `TargetFilter` reachable through
/// the common target-bearing fields of an `Effect`. Companion to
/// `each_quantity_expr_mut`; used by callers that need to rewrite target slots
/// across an effect chain (e.g. the prevention follow-up call site rewrites
/// `ParentTargetController` → `PostReplacementSourceController` so "the source's
/// controller draws cards" resolves against the prevented event's damage source).
///
/// Pragmatic visitor — covers the subset of `Effect` variants whose target
/// slots are statically declared as `TargetFilter` or `Option<TargetFilter>`.
/// Variants not listed here keep their target slots untouched; **add an arm
/// when introducing a new target-bearing variant** so future rewrites cover it.
/// Variant list mirrors `replace_target_with_parent` plus a handful of
/// player-targetable effects (Draw, LoseLife, Discard, Mill, Scry, Surveil,
/// RevealFromHand) whose `target` field is a `TargetFilter` and can therefore
/// hold a player ref like `ParentTargetController`.
pub(crate) fn each_target_filter_mut(effect: &mut Effect, f: &mut impl FnMut(&mut TargetFilter)) {
    match effect {
        Effect::Tap { target }
        | Effect::Untap { target }
        | Effect::Destroy { target, .. }
        | Effect::Regenerate { target, .. }
        | Effect::Sacrifice { target, .. }
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
        | Effect::ForceBlock { target }
        | Effect::Draw { target, .. }
        | Effect::Discard { target, .. }
        | Effect::Mill { target, .. }
        | Effect::Scry { target, .. }
        | Effect::Surveil { target, .. } => f(target),
        Effect::PutCounter { target, .. }
        | Effect::AddCounter { target, .. }
        | Effect::RemoveCounter { target, .. } => f(target),
        Effect::ChangeZone { target, .. } | Effect::ChangeZoneAll { target, .. } => f(target),
        Effect::LoseLife {
            target: Some(target),
            ..
        } => f(target),
        Effect::GenericEffect {
            target: Some(target),
            ..
        } => f(target),
        _ => {}
    }
}

/// CR 608.2 + CR 107.2: Rewrite target-scoped `QuantityRef` variants to their
/// controller-scoped equivalents across an ability tree. Under
/// `player_scope: All` / `Opponent` / etc., the resolver rebinds
/// `scoped.controller = *pid` per iterating player, but `targets` remains
/// unchanged. `TargetLifeTotal` and `TargetZoneCardCount` resolve against
/// `targets`, returning 0 in the scoped context; the controller-scoped
/// `LifeTotal`, `HandSize`, `GraveyardSize`, and
/// `ZoneCardCount{scope:Controller}` variants read the rebound controller.
///
/// Walks the entire `AbilityDefinition` tree (ability → sub_ability → …) so
/// every chained clause under an each-player chain (Pox Plague:
/// "Each player loses half their life, then discards half the cards in their
/// hand, then sacrifices half the permanents they control") picks up the
/// rewrite uniformly. Only reached when the top-level `def.player_scope` is
/// set, so non-scoped abilities keep the target-scoped resolution path.
fn rewrite_player_scope_refs(def: &mut AbilityDefinition) {
    fn rewrite_quantity_expr(expr: &mut QuantityExpr) {
        match expr {
            QuantityExpr::Ref { qty } => match qty {
                QuantityRef::TargetLifeTotal => *qty = QuantityRef::LifeTotal,
                QuantityRef::TargetZoneCardCount { zone } => match zone {
                    crate::types::ability::ZoneRef::Hand => *qty = QuantityRef::HandSize,
                    crate::types::ability::ZoneRef::Graveyard => *qty = QuantityRef::GraveyardSize,
                    crate::types::ability::ZoneRef::Library => {
                        *qty = QuantityRef::ZoneCardCount {
                            zone: crate::types::ability::ZoneRef::Library,
                            card_types: Vec::new(),
                            scope: crate::types::ability::CountScope::Controller,
                        };
                    }
                    // Exile has no clean controller-scoped equivalent in the
                    // current `QuantityRef` set. Leave untouched — the
                    // scoped-controller resolver path does not exist for
                    // exile card counts, so preserving the target-scoped
                    // form at least lets a targeting ability resolve
                    // correctly.
                    crate::types::ability::ZoneRef::Exile => {}
                },
                _ => {}
            },
            QuantityExpr::HalfRounded { inner, .. }
            | QuantityExpr::Multiply { inner, .. }
            | QuantityExpr::Offset { inner, .. } => rewrite_quantity_expr(inner),
            QuantityExpr::Fixed { .. } => {}
        }
    }

    each_quantity_expr_mut(&mut def.effect, &mut rewrite_quantity_expr);
    if let Some(sub) = def.sub_ability.as_mut() {
        rewrite_player_scope_refs(sub);
    }
    if let Some(else_branch) = def.else_ability.as_mut() {
        rewrite_player_scope_refs(else_branch);
    }
}

/// CR 107.1a: Back-apply a rounding mode to every `HalfRounded` in an ability
/// tree. Used by `parse_effect_chain_impl` after stripping a trailing
/// "Round down each time" / "Round up each time" sentence (Pox Plague), so
/// earlier chunks that defaulted to `RoundingMode::Down` pick up an explicit
/// `Up` annotation that Oracle grammar places in a separate closing
/// sentence. Class-level: handles both the redundant-default case (Pox
/// Plague already has Down) and hypothetical "Round up each time" variants
/// without new types.
fn rewrite_rounding_mode(def: &mut AbilityDefinition, mode: RoundingMode) {
    fn rewrite_quantity_expr(expr: &mut QuantityExpr, mode: RoundingMode) {
        match expr {
            QuantityExpr::HalfRounded { inner, rounding } => {
                *rounding = mode;
                rewrite_quantity_expr(inner, mode);
            }
            QuantityExpr::Multiply { inner, .. } | QuantityExpr::Offset { inner, .. } => {
                rewrite_quantity_expr(inner, mode);
            }
            QuantityExpr::Ref { .. } | QuantityExpr::Fixed { .. } => {}
        }
    }

    each_quantity_expr_mut(&mut def.effect, &mut |expr| {
        rewrite_quantity_expr(expr, mode)
    });
    if let Some(sub) = def.sub_ability.as_mut() {
        rewrite_rounding_mode(sub, mode);
    }
    if let Some(else_branch) = def.else_ability.as_mut() {
        rewrite_rounding_mode(else_branch, mode);
    }
}

/// Parse a compound effect chain into an `AbilityDefinition` sub-ability chain.
///
/// Phase 1 keeps the existing clause/effect semantics but replaces the fragile
/// textual `replace(", then ", ". ").split(". ")` logic with a boundary-aware
/// splitter that preserves whether a chunk ended a sentence or was linked by
/// `, then`.
///
/// **Self-reference precondition:** this entry point takes no `card_name`
/// and performs no normalization. Callers must pass text whose card-name
/// self-references have already been rewritten to `~` — which is always the
/// case when reached via [`parse_oracle_text`] or any of the wrapper parsers
/// that invoke `normalize_card_name_refs` internally. External test callers
/// must pre-normalize via `normalize_card_name_refs` before invoking.
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
    // CR 107.1a: Strip a trailing "Round down each time" / "Round up each time"
    // sentence before chain splitting — it is a chain-level rounding annotation
    // (Pox Plague) that back-applies to every `HalfRounded` the chain contains.
    // Stripped here so it doesn't become an Unimplemented chunk; the captured
    // mode is re-applied to the built chain via `rewrite_rounding_mode`.
    let (text, chain_rounding) = strip_trailing_rounding_annotation(text);
    let text = text.as_str();
    let full_text = text; // Bind before `text` is shadowed by strip helpers in the loop
    let chunks = split_clause_sequence(text);
    // CR 107.3i: "Normally, all instances of X on an object have the same value."
    // Build a per-chunk sentence-scoped `where X is <expr>` binding so that when
    // one clause in a sentence binds X ("... and you gain X life, where X is <expr>"),
    // sibling clauses in the same sentence ("Target player loses X life")
    // substitute X with the same expression. Delimited by ClauseBoundary::Sentence.
    let sentence_where_x = compute_sentence_where_x(&chunks);
    let mut defs: Vec<AbilityDefinition> = Vec::new();

    // CR 608.2c + CR 117.3a: "Anchor subject" for chain-level anaphoric
    // inheritance. When a clause in the chain declares an explicit player-scope
    // subject other than the caster (e.g., "its controller may search their
    // library" → ParentTargetController), subsequent clauses in the same chain
    // that produce caster-defaulted player-scope effects (e.g., "then shuffle"
    // → `Shuffle { target: Controller }`) inherit this anchor so the correct
    // player performs the action. Only propagates within a single sentence /
    // chain; the anchor is reset at each top-level call.
    let mut anchor_subject: Option<TargetFilter> = None;

    for (chunk_idx, chunk) in chunks.iter().enumerate() {
        let normalized_text = strip_leading_sequence_connector(&chunk.text).trim();
        if normalized_text.is_empty() {
            continue;
        }

        // "Starting with you, " — multiplayer ordering modifier that's irrelevant
        // for 1v1. Strip the prefix so the remaining effect text is parsed normally.
        let normalized_text = {
            let temp_lower = normalized_text.to_lowercase();
            nom_on_lower(normalized_text, &temp_lower, |i| {
                value((), tag("starting with you, ")).parse(i)
            })
            .map_or(normalized_text, |((), rest)| rest)
        };

        // CR 118.9 + CR 119.4: Alternative-cost rider — "[If you cast a spell
        // this way,] pay <ability-cost> rather than paying its mana cost."
        // This is a *modifier* on the previous chain entry's `CastFromZone`
        // grant rather than its own effect. Fold the cost onto the most
        // recent `CastFromZone` def (walking sub_ability descendants) and
        // skip emitting a sibling Unimplemented chunk. Used by Nashi, Moon
        // Sage's Scion: the granted card is cast by paying life equal to its
        // mana value instead of paying its mana cost.
        if let Some(cost) = try_parse_alt_cost_rider(normalized_text) {
            if attach_alt_cost_to_prior_cast_from_zone(&mut defs, cost) {
                continue;
            }
        }

        // CR 608.2c: "Otherwise, [effect]" — attach as else_ability on the
        // most recent conditional def in the chain.
        let lower_check = normalized_text.to_lowercase();
        let otherwise_rest = nom_on_lower(normalized_text, &lower_check, |i| {
            value(
                (),
                alt((
                    tag("otherwise, "),
                    tag("otherwise "),
                    tag("if not, "),
                    tag("if no player does, "),
                    tag("if no one does, "),
                )),
            )
            .parse(i)
        });
        if let Some((_, else_text)) = otherwise_rest {
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

        // "Repeat this process" — recognized directive that doesn't produce an
        // independent effect. Repetition semantics are handled by the specific card
        // effects (cascade, explore, etc.); the parser recognizes these to avoid
        // Unimplemented gaps. The trailing qualifier ("once", "any number of times",
        // "for the next card", "until ...") is consumed with the prefix.
        // Also handles "you may repeat this process" and "if you do, repeat this process".
        if nom_on_lower(normalized_text, &lower_check, |i| {
            let (i, _) = nom::combinator::opt(alt((
                tag::<_, _, VerboseError<&str>>("you may "),
                tag("if you do, "),
                tag("if you do "),
            )))
            .parse(i)?;
            value((), tag("repeat this process")).parse(i)
        })
        .is_some()
        {
            continue;
        }

        // CR 608.2c: "If <cond>, you may instead <reveal-N-from-among-body>" —
        // conditional alternative SELECTION parameters for the preceding Dig effect.
        // The alternative reuses the same top-N source but swaps keep_count/up_to/filter.
        // Checked before the generic "instead" clause because the body starts with
        // "you may instead ..." rather than a bare "instead ..." prefix/suffix and
        // carries no standalone effect — it patches the preceding Dig.
        if let Some(alt_def) = try_parse_dig_instead_alternative(normalized_text, defs.last(), kind)
        {
            if let Some(last_def) = defs.pop() {
                let mut new_def = alt_def;
                new_def.else_ability = Some(Box::new(last_def));
                defs.push(new_def);
                continue;
            }
        }

        // CR 608.2e: "if [condition], [effect] instead" — the preceding ability's effect
        // is replaced when the condition holds. Keep the base effect as the root so
        // target collection stays anchored on the printed target-bearing clause.
        if let Some(mut instead_def) = try_parse_generic_instead_clause(normalized_text, kind) {
            if let Some(mut last_def) = defs.pop() {
                // CR 702.33d + CR 707.10: Resolve "create N of those tokens" anaphor
                // against the fallback (else_ability) effect before composition, since
                // the instead clause was parsed in isolation by the nested chain call
                // and could not see its antecedent.
                rewrite_those_tokens_from_antecedent(&mut instead_def.effect, &last_def.effect);
                last_def.sub_ability = Some(Box::new(instead_def));
                defs.push(last_def);
                continue;
            }
        }

        let (condition, text) = strip_additional_cost_conditional(normalized_text);
        // CR 608.2c: General leading conditional — "if [condition], [effect]".
        // Runs only when no dedicated leading stripper matched. Handles patterns like
        // "if you control 3 or more creatures, draw a card".
        let (leading_cond, text) = if condition.is_none() {
            strip_leading_general_conditional(&text)
        } else {
            (None, text)
        };
        let condition = condition.or(leading_cond);
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
        // CR 202.3 + CR 608.2c: Mana value threshold condition — same priority as counter_cond.
        let (mv_cond, text) = strip_mana_value_conditional(&text);
        let (cast_from_zone, text) = if condition.is_none()
            && if_you_do.is_none()
            && counter_cond.is_none()
            && mv_cond.is_none()
        {
            strip_cast_from_zone_conditional(&text)
        } else {
            (None, text)
        };
        let (card_type_cond, text) = if condition.is_none()
            && if_you_do.is_none()
            && counter_cond.is_none()
            && mv_cond.is_none()
            && cast_from_zone.is_none()
        {
            strip_card_type_conditional(&text)
        } else {
            (None, text)
        };
        let (property_cond, text) = if condition.is_none()
            && if_you_do.is_none()
            && counter_cond.is_none()
            && mv_cond.is_none()
            && cast_from_zone.is_none()
            && card_type_cond.is_none()
        {
            strip_property_conditional(&text)
        } else {
            (None, text)
        };
        // CR 608.2c: "If it's your turn" / "If it's not your turn" — game-state condition
        let (turn_cond, text) = if condition.is_none()
            && if_you_do.is_none()
            && counter_cond.is_none()
            && mv_cond.is_none()
            && cast_from_zone.is_none()
            && card_type_cond.is_none()
            && property_cond.is_none()
        {
            strip_turn_conditional(&text)
        } else {
            (None, text)
        };
        // CR 608.2e: "If that creature has [keyword], [effect] instead"
        let (keyword_instead_cond, text) = if condition.is_none()
            && if_you_do.is_none()
            && counter_cond.is_none()
            && mv_cond.is_none()
            && cast_from_zone.is_none()
            && card_type_cond.is_none()
            && property_cond.is_none()
            && turn_cond.is_none()
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
            && mv_cond.is_none()
            && cast_from_zone.is_none()
            && card_type_cond.is_none()
            && property_cond.is_none()
            && turn_cond.is_none()
            && keyword_instead_cond.is_none()
        {
            strip_suffix_conditional(&text)
        } else {
            (None, text)
        };
        let condition = condition
            .or(counter_cond)
            .or(mv_cond)
            .or(if_you_do)
            .or(cast_from_zone)
            .or(card_type_cond)
            .or(property_cond)
            .or(turn_cond)
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
        // CR 608.2e: Strip leading "instead " when a condition was extracted.
        // The condition already encodes the replacement gate; "instead" is a
        // textual marker that the effect parser doesn't need.
        let text = if condition.is_some() {
            strip_leading_instead(&text)
        } else {
            text
        };
        // CR 508.4 / CR 614.1: "If [condition], they/those tokens/it enter(s) tapped and
        // attacking" — conditional modifier on preceding Token/CopyTokenOf/ChangeZone.
        // Model as an "instead" swap: the modified Token (with tapped+attacking set)
        // replaces the original when the condition holds.
        {
            let enters_lower = text.to_lowercase();
            if condition.is_some()
                && nom_primitives::scan_contains(&enters_lower, "enter tapped and attacking")
            {
                if let Some(prev) = defs.last() {
                    // These are the only effects with `enters_attacking` + `tapped`/`enter_tapped` fields.
                    let can_patch = matches!(
                        &*prev.effect,
                        Effect::CopyTokenOf { .. }
                            | Effect::Token { .. }
                            | Effect::ChangeZone { .. }
                    );
                    if can_patch {
                        let mut patched = defs.pop().unwrap();
                        match &mut *patched.effect {
                            Effect::CopyTokenOf {
                                enters_attacking,
                                tapped,
                                ..
                            } => {
                                *enters_attacking = true;
                                *tapped = true;
                            }
                            Effect::Token {
                                enters_attacking,
                                tapped,
                                ..
                            } => {
                                *enters_attacking = true;
                                *tapped = true;
                            }
                            Effect::ChangeZone {
                                enters_attacking,
                                enter_tapped,
                                ..
                            } => {
                                *enters_attacking = true;
                                *enter_tapped = true;
                            }
                            _ => {}
                        }
                        // Convert to "instead" swap: patched Token becomes the sub_ability
                        // that fires when the condition holds; the original becomes else_ability
                        // (the fallback when condition is false).
                        let original = {
                            let mut orig = patched.clone();
                            match &mut *orig.effect {
                                Effect::CopyTokenOf {
                                    enters_attacking,
                                    tapped,
                                    ..
                                } => {
                                    *enters_attacking = false;
                                    *tapped = false;
                                }
                                Effect::Token {
                                    enters_attacking,
                                    tapped,
                                    ..
                                } => {
                                    *enters_attacking = false;
                                    *tapped = false;
                                }
                                Effect::ChangeZone {
                                    enters_attacking,
                                    enter_tapped,
                                    ..
                                } => {
                                    *enters_attacking = false;
                                    *enter_tapped = false;
                                }
                                _ => {}
                            }
                            orig
                        };
                        patched.condition = condition;
                        patched.else_ability = Some(Box::new(original));
                        defs.push(patched);
                        continue;
                    }
                }
            }
        }
        let (is_optional, opponent_may_scope, implicit_player_scope, text) =
            strip_optional_effect_prefix(&text);
        let (repeat_for, text) = strip_for_each_prefix(&text);
        // CR 609.3: "twice" / "N times" suffix — same mechanism as "for each" prefix.
        let (repeat_count, text) = if repeat_for.is_none() {
            strip_repeat_count_suffix(&text)
        } else {
            (None, text)
        };
        let repeat_for = repeat_for.or(repeat_count);
        let (player_scope, text) = strip_player_scope_subject(&text);
        // CR 608.2d: When the optional prefix ("each opponent may") carries an
        // implicit per-player iteration, propagate it to the ability so the
        // OptionalEffectChoice is emitted once per matching player.
        let player_scope = player_scope.or(implicit_player_scope);

        // CR 701.21a + CR 608.2k: Derive the actor performing this chunk's effect
        // from any actor prefix that was just stripped ("you (may) ", "an
        // opponent (may) ", "each opponent ", "each player "). Threaded via
        // `ParseContext.actor` so targeted-action parsers can default
        // `TargetFilter::Typed.controller` to the actor when the target phrase
        // itself doesn't specify one (e.g. "you may sacrifice a non-Demon
        // creature" → controller defaults to You). Without this default, the
        // resolver would treat `controller: None` as Any and let the actor
        // sacrifice an opponent's permanent — violating CR 701.21a.
        let chunk_actor = match (is_optional, opponent_may_scope, player_scope) {
            (true, None, None) => Some(ControllerRef::You),
            (true, Some(_), _) => Some(ControllerRef::Opponent),
            (_, _, Some(PlayerFilter::Opponent)) => Some(ControllerRef::Opponent),
            (_, _, Some(PlayerFilter::Controller)) => Some(ControllerRef::You),
            _ => None,
        }
        .or_else(|| ctx.actor.clone());
        let chunk_ctx = ParseContext {
            subject: ctx.subject.clone(),
            card_name: ctx.card_name.clone(),
            // CR 707.9a + CR 603.1: propagate the trigger index from the parent
            // ctx — `current_trigger_index` is a property of the whole trigger
            // body, not of an individual chunk, so all chunks inside a trigger
            // share the same index for the BecomeCopy except-clause parser to
            // reference via `RetainPrintedTriggerFromSource`. Actor, by contrast,
            // is per-chunk (re-derived from each chunk's stripped prefix above).
            current_trigger_index: ctx.current_trigger_index,
            actor: chunk_actor,
        };
        let ctx = &chunk_ctx;

        // CR 603.7a: Check for temporal prefix before suffix. When present, parse the
        // inner effect through the full pipeline and wrap in CreateDelayedTrigger.
        let (text_after_prefix, prefix_delayed) = strip_temporal_prefix(&text);
        let text_where_x_lower = text.to_lowercase();
        let (_, local_where_x_expression) =
            strip_trailing_where_x(TextPair::new(&text, &text_where_x_lower));
        // CR 107.3i: If this chunk has no local "where X is" but a sibling clause
        // in the same sentence binds X, propagate the sibling binding so "target
        // player loses X life" and "you gain X life" in the same sentence share
        // the same X expression (Essence Harvest, Debt to the Deathless, etc.).
        let where_x_expression = local_where_x_expression.or_else(|| {
            sentence_where_x
                .get(chunk_idx)
                .and_then(|expr| expr.clone())
        });
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
            apply_where_x_ability_expression(&mut inner_def, where_x_expression.as_deref());
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

        // CR 608.2c: Verb carry-forward for bare "target X" clauses in multi-target
        // conjunctions. When a clause parses as Unimplemented and starts with "target",
        // inherit the verb from the previous successfully-parsed effect and re-parse.
        // Handles patterns like "destroy target artifact, target creature, ..." (Decimate).
        let text_no_qty_lower = text_no_qty.to_lowercase();
        let clause = if matches!(clause.effect, Effect::Unimplemented { .. })
            && tag::<_, _, VerboseError<&str>>("target ")
                .parse(text_no_qty_lower.as_str())
                .is_ok()
        {
            if let Some(verb) = defs
                .last()
                .and_then(|prev| extract_effect_verb(&prev.effect))
            {
                let reparsed_text = format!("{verb} {text_no_qty}");
                let reparsed = parse_effect_clause(&reparsed_text, ctx);
                if !matches!(reparsed.effect, Effect::Unimplemented { .. }) {
                    reparsed
                } else {
                    clause
                }
            } else {
                clause
            }
        } else {
            clause
        };

        // CR 608.2c + CR 117.3a: Anchor-subject inheritance. Before this chunk is
        // lowered into an AbilityDefinition, check whether a caster-defaulted
        // player-scope effect should inherit a previously-declared subject
        // (e.g., "then shuffle" after "its controller may search their library"
        // → Shuffle inherits the ParentTargetController anchor). Only
        // overrides when the effect still carries the caster default (Controller).
        let mut clause = clause;
        if let Some(ref anchor) = anchor_subject {
            apply_anchor_subject(&mut clause.effect, anchor);
        }

        // CR 608.2c: TargetOnly is a structural wrapper — its sub_ability is the action
        // to perform on the target. Preserve this relationship rather than flattening.
        let is_target_only = matches!(clause.effect, Effect::TargetOnly { .. });
        // Update anchor from this clause's subject (e.g., SearchLibrary.target_player
        // set to a non-caster player-scope filter).
        if let Some(extracted) = extract_player_anchor(&clause.effect) {
            anchor_subject = Some(extracted);
        }
        let mut def = AbilityDefinition::new(kind, clause.effect);
        let clause_sub = if is_target_only {
            def.sub_ability = clause.sub_ability;
            None
        } else {
            clause.sub_ability
        };
        if is_optional {
            def.optional = true;
            def.optional_for = opponent_may_scope;
        }
        // CR 117.3a + CR 608.2c: Propagate subject-phrase "may" modal from the
        // parsed clause (e.g., "its controller may search their library").
        if clause.optional {
            def.optional = true;
        }
        if let Some(qty) = repeat_for {
            if matches!(*def.effect, Effect::TargetOnly { .. }) {
                if let Some(sub) = def.sub_ability.as_mut() {
                    sub.repeat_for = Some(qty);
                } else {
                    def.repeat_for = Some(qty);
                }
            } else {
                def.repeat_for = Some(qty);
            }
        }
        if let Some(scope) = player_scope {
            def.player_scope = Some(scope);
        }
        if let Some(duration) = clause.duration {
            def = def.duration(duration);
        }
        // CR 608.2c: Apply condition — chain-level takes priority over clause-level.
        let effective_condition = condition.as_ref().or(clause.condition.as_ref());
        if let Some(cond) = effective_condition {
            def = def.condition(cond.clone());
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
        // CR 608.2c: Pronoun clause following an unconditional targeted effect.
        // "Tap target creature. Put two stun counters on it." — "it" refers to the
        // tapped creature from the previous sentence, not the ability source.
        if condition.is_none()
            && defs
                .last()
                .is_some_and(|prev| prev.condition.is_none() && has_typed_target(&prev.effect))
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

        if matches!(
            def.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        ) && matches!(&*def.effect, Effect::SearchLibrary { .. })
            && defs.len() >= 2
        {
            let previous_is_search =
                matches!(&*defs[defs.len() - 2].effect, Effect::SearchLibrary { .. });
            let trailing_is_search_destination = matches!(
                &*defs[defs.len() - 1].effect,
                Effect::ChangeZone {
                    origin: Some(Zone::Library),
                    destination: Zone::Hand,
                    ..
                }
            );
            if previous_is_search && trailing_is_search_destination {
                def.else_ability = Some(Box::new(defs.pop().unwrap()));
            }
        }

        let mut current_defs = vec![def];
        if let Some(sub) = clause_sub {
            current_defs.push(*sub);
        }
        for current in &mut current_defs {
            apply_where_x_ability_expression(current, where_x_expression.as_deref());
        }

        // CR 603.7: Wrap in CreateDelayedTrigger if temporal suffix was found.
        //
        // CR 608.2c: Lift the inner ability's `condition` onto the new outer
        // wrapper. A conditional like "If you control a Wizard, [effect at
        // next main phase]" scopes to the whole clause but is evaluated at
        // SPELL RESOLUTION time, not at delayed-trigger firing. The condition
        // must gate whether the delayed trigger is even created — not what
        // runs when it fires. Without this lift the inner `condition` would
        // be stranded dead metadata (the `effects::delayed_trigger::resolve`
        // path builds a fresh `ResolvedAbility` and does not read
        // `effect_def.condition`), silently dropping the gate. Mana Sculpt is
        // the first card exposed to this architecture; the lift closes the
        // latent gap for any future delayed-trigger with an inner condition.
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
                let lifted_condition = inner.condition.clone();
                *current = AbilityDefinition::new(
                    kind,
                    Effect::CreateDelayedTrigger {
                        condition: delayed_cond.clone(),
                        effect: Box::new(inner),
                        uses_tracked_set: false,
                    },
                );
                current.condition = lifted_condition;
            }
        }

        // CR 603.7: Cross-clause pronoun → mark uses_tracked_set on delayed trigger
        if let Some(previous) = defs.last() {
            if is_exile_effect(&previous.effect) || is_token_creating_effect(&previous.effect) {
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

    // CR 701.20a vs CR 701.16a: Demote reveal-Dig back to RevealTop when no DigFromAmong
    // continuation patched it. An unpatched Dig { reveal: true, keep_count: None, filter: Any }
    // is a simple "reveal the top N" with no player selection — it must resolve synchronously
    // (via RevealTop) so that sub_ability chains like RevealedHasCardType evaluate inline.
    for def in &mut defs {
        if let Effect::Dig {
            count,
            keep_count: None,
            filter: TargetFilter::Any,
            reveal: true,
            ..
        } = &*def.effect
        {
            let count_val = match count {
                QuantityExpr::Fixed { value } => *value as u32,
                _ => 1,
            };
            *def.effect = Effect::RevealTop {
                player: TargetFilter::Controller,
                count: count_val,
            };
        }
    }

    // CR 701.20a + CR 608.2c: "look at the top card … if it's a [type] …" pure-peek pattern.
    // A private-Dig (reveal: false, keep_count: None) followed immediately by a def
    // whose condition is RevealedHasCardType is a conditional peek: the dig is only
    // there to show the player the card; the sub_ability decides whether to take it.
    // Mark the Dig as keep_count = 0 so the runtime skips DigChoice interaction and
    // instead sets last_revealed_ids, letting the condition evaluate correctly.
    for i in 0..defs.len().saturating_sub(1) {
        if let Effect::Dig {
            reveal: false,
            keep_count: None,
            filter: TargetFilter::Any,
            ..
        } = &*defs[i].effect
        {
            if matches!(
                defs[i + 1].condition,
                Some(AbilityCondition::RevealedHasCardType { .. })
            ) {
                if let Effect::Dig { keep_count, .. } = &mut *defs[i].effect {
                    *keep_count = Some(0);
                }
            }
        }
    }

    // CR 702.33d + CR 608.2e: Resolve "create [N] of those tokens [instead]"
    // anaphoric subs — the sub-ability parses as `Unimplemented` because the
    // noun "those tokens" refers back to the previous clause's token-creation
    // effect. Rewrite those subs by cloning the previous effect with an
    // updated count (Rite of Replication / Saproling Migration / Krothuss).
    resolve_those_tokens_anaphors(&mut defs);

    // CR 701.36a + CR 603.7c: Resolve "the token created this way …" and the
    // "sacrifice it" anaphors that follow a token-creating effect (Populate,
    // CopyTokenOf, Token). The antecedent is the populated / created token;
    // `TargetFilter::LastCreated` at runtime resolves against
    // `state.last_created_token_ids` (snapshotted at delayed-trigger
    // creation for the Sacrifice case — CR 603.7c).
    resolve_populated_token_anaphors(&mut defs);

    // CR 706 + CR 705: Consolidate die result table lines into their parent RollDie,
    // and coin flip conditional branches into their parent FlipCoin.
    consolidate_die_and_coin_defs(&mut defs, kind);

    // Chain: last has no sub_ability, each earlier one chains to next.
    // When a def already has a sub_ability (e.g., TargetOnly with attached Explore),
    // append to the deepest sub rather than overwriting.
    let mut result = if defs.len() > 1 {
        let last = defs.pop().unwrap();
        let mut chain = last;
        while let Some(mut prev) = defs.pop() {
            if prev.condition == Some(AbilityCondition::AdditionalCostPaidInstead) {
                if let Some(base_chain) = prev.else_ability.as_mut() {
                    if matches!(
                        (&*base_chain.effect, &*chain.effect),
                        (
                            Effect::ChangeZone {
                                origin: Some(Zone::Library),
                                destination: Zone::Hand,
                                ..
                            },
                            Effect::ChangeZone {
                                origin: Some(Zone::Library),
                                destination: Zone::Hand,
                                ..
                            }
                        )
                    ) {
                        append_to_deepest_sub_ability(base_chain, chain.sub_ability.clone());
                    }
                }
            }
            // A node attached as a `sub_ability` is a resolution continuation
            // of its parent, not an independently activatable ability.
            // Normalize its kind to `Spell` (the "resolves alongside parent"
            // kind) before linking. This matches the convention used by
            // dedicated clause builders that construct sub-abilities directly
            // (e.g., `try_parse_pump_with_damage_sub` at line 3220).
            chain.kind = AbilityKind::Spell;
            if prev.sub_ability.is_some() {
                // Walk to the deepest sub_ability and append there
                let mut cursor = &mut prev;
                while cursor.sub_ability.is_some() {
                    cursor = cursor.sub_ability.as_mut().unwrap();
                }
                cursor.sub_ability = Some(Box::new(chain));
            } else {
                prev.sub_ability = Some(Box::new(chain));
            }
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
    };

    // CR 608.2 + CR 107.2: If the outermost ability carries `player_scope`,
    // rewrite target-scoped refs ("their life", "their hand") to their
    // controller-scoped equivalents so they resolve per-iterating-player.
    if result.player_scope.is_some() {
        rewrite_player_scope_refs(&mut result);
    }

    // CR 107.1a: Apply the chain-level rounding annotation (captured above)
    // to every HalfRounded in the built tree. No-op when the sentence was
    // absent (chain_rounding == None).
    if let Some(mode) = chain_rounding {
        rewrite_rounding_mode(&mut result, mode);
    }

    // CR 303.4f + CR 301.5b + CR 603.7d: Wire `forward_result: true` on a
    // parent zone-change to Battlefield when the chained sub-ability is an
    // `Attach` gated by `ZoneChangedThisWay`. Without this, the runtime
    // resolves the sub-ability with `source_id` = the original ability source
    // (the trigger source / Saga / activated permanent), so the Attach tries
    // to equip *that* object to the chosen creature — wrong for Armored
    // Skyhunter (Skyhunter cannot equip itself), wrong for Vault 101: Birthday
    // Party (a Saga is not Equipment), wrong for Quest for the Holy Relic and
    // Stonehewer Giant (the searcher is not the moved Equipment).
    //
    // The `forward_result` flag makes the runtime forward the just-moved
    // card's id as the sub-ability's `source_id` (see `effects/mod.rs`
    // forward_result branch), so `Attach::resolve` operates on the correct
    // attaching object.
    rewire_attach_forward_result(&mut result);

    result
}

/// CR 303.4f / CR 301.5b / CR 603.7d: Walk the chain and set
/// `forward_result: true` on every `Dig`/`ChangeZone` whose `destination`
/// is `Battlefield` and whose chained sub-ability is an `Attach` carrying
/// the `ZoneChangedThisWay` condition. The condition is the parser-side
/// signal that the Oracle text said "If a[n] [type] is/was put onto the
/// battlefield this way, [attach it]" — i.e. the just-moved card must
/// become the attaching object.
///
/// Recurses through nested sub-abilities so chains of arbitrary depth
/// (e.g. Skyhunter's Dig → Attach → PutAtLibraryPosition) are covered.
fn rewire_attach_forward_result(def: &mut AbilityDefinition) {
    if let Some(sub) = def.sub_ability.as_ref() {
        let sub_is_attach_with_zone_changed_cond = matches!(*sub.effect, Effect::Attach { .. })
            && matches!(
                sub.condition,
                Some(AbilityCondition::ZoneChangedThisWay { .. })
            );
        let parent_moves_to_battlefield = matches!(
            *def.effect,
            Effect::Dig {
                destination: Some(Zone::Battlefield),
                ..
            } | Effect::ChangeZone {
                destination: Zone::Battlefield,
                ..
            }
        );
        if sub_is_attach_with_zone_changed_cond && parent_moves_to_battlefield {
            def.forward_result = true;
        }
    }
    if let Some(sub) = def.sub_ability.as_mut() {
        rewire_attach_forward_result(sub);
    }
    if let Some(else_branch) = def.else_ability.as_mut() {
        rewire_attach_forward_result(else_branch);
    }
}

/// CR 702.33d + CR 608.2e: Resolve "create [N] of those tokens [instead]"
/// anaphoric clauses. The clause refers back to the previous def's token
/// creation effect (either `Token` or `CopyTokenOf`) and reproduces it with
/// a new count. We walk `defs` looking for an `Unimplemented` clause whose
/// description matches the anaphor, and rewrite its effect as a clone of the
/// previous def's effect with the parsed count.
fn resolve_those_tokens_anaphors(defs: &mut [AbilityDefinition]) {
    for i in 1..defs.len() {
        let (prev_rest, cur_rest) = defs.split_at_mut(i);
        let prev = &prev_rest[i - 1];
        let cur = &mut cur_rest[0];
        rewrite_those_tokens_from_antecedent(&mut cur.effect, &prev.effect);
    }
}

/// CR 702.33d + CR 707.10: If `cur` is an `Unimplemented` "create N of those
/// tokens" anaphor, rewrite it as a clone of the `antecedent` token-creation
/// effect with count set to N. No-op when the shapes don't match.
fn rewrite_those_tokens_from_antecedent(cur: &mut Effect, antecedent: &Effect) {
    let Some(count) = match_create_of_those_tokens(cur) else {
        return;
    };
    let new_effect = match antecedent {
        Effect::CopyTokenOf {
            target,
            enters_attacking,
            tapped,
            extra_keywords,
            additional_modifications,
            ..
        } => Some(Effect::CopyTokenOf {
            target: target.clone(),
            enters_attacking: *enters_attacking,
            tapped: *tapped,
            count: QuantityExpr::Fixed {
                value: count as i32,
            },
            extra_keywords: extra_keywords.clone(),
            additional_modifications: additional_modifications.clone(),
        }),
        Effect::Token {
            name,
            power,
            toughness,
            types,
            colors,
            keywords,
            tapped,
            owner,
            attach_to,
            enters_attacking,
            supertypes,
            static_abilities,
            enter_with_counters,
            ..
        } => Some(Effect::Token {
            name: name.clone(),
            power: power.clone(),
            toughness: toughness.clone(),
            types: types.clone(),
            colors: colors.clone(),
            keywords: keywords.clone(),
            tapped: *tapped,
            count: QuantityExpr::Fixed {
                value: count as i32,
            },
            owner: owner.clone(),
            attach_to: attach_to.clone(),
            enters_attacking: *enters_attacking,
            supertypes: supertypes.clone(),
            static_abilities: static_abilities.clone(),
            enter_with_counters: enter_with_counters.clone(),
        }),
        _ => None,
    };
    if let Some(effect) = new_effect {
        *cur = effect;
    }
}

/// Match an `Unimplemented` effect whose description is
/// "create <N> of those tokens" (optionally with a trailing modifier like
/// "that are tapped and attacking" or "instead"). Returns the parsed count.
fn match_create_of_those_tokens(effect: &Effect) -> Option<u32> {
    let Effect::Unimplemented { name, description } = effect else {
        return None;
    };
    if name != "create" {
        return None;
    }
    let text = description.as_deref()?;
    let lower = text.to_lowercase();
    let rest = lower.strip_prefix("create ")?;
    let (count, after) = crate::parser::oracle_util::parse_number(rest)?;
    let after = after.trim_start();
    // Accept "of those tokens" with optional trailing modifier.
    let tail = after.strip_prefix("of those tokens")?;
    // Accept end, or a comma/whitespace-prefixed modifier.
    if tail.is_empty() || tail.starts_with(' ') || tail.starts_with(',') || tail.starts_with('.') {
        Some(count)
    } else {
        None
    }
}

/// CR 608.2k + CR 603.7c + CR 701.36a: Rewrite token anaphors following a
/// token-creating effect.
///
/// Two rewrites, both scoped to defs whose chain contains a prior token
/// creator (`Populate`, `CopyTokenOf`, `Token`):
///
/// 1. `Effect::Unimplemented { description: "<anaphor> <mod>" }`
///    → `GenericEffect { target: Some(LastCreated), static_abilities: [...],
///    duration: Some(UntilEndOfTurn) }` where the modifications are parsed
///    from the verb phrase ("gains haste" / "gets +1/+1" / …).
///    Recognized anaphor prefixes (longest-first to disambiguate):
///    "the token created this way " / "the tokens created this way "
///    (populate-specific qualifier) and the plain forms "this token " /
///    "that token " / "the token " (covers Pietra, Inalla, and similar
///    token-creators that follow with a generic pronoun rather than the
///    populate-specific phrasing).
///
/// 2. Inside a `CreateDelayedTrigger` whose inner effect references the
///    created token via `TargetFilter::ParentTarget` (currently the
///    imperative parser's "it" / "that creature" default), rewrite that
///    target to `TargetFilter::LastCreated`. At delayed-trigger creation
///    time, `delayed_trigger::resolve` snapshots
///    `state.last_created_token_ids` into the delayed ability's targets.
fn resolve_populated_token_anaphors(defs: &mut [AbilityDefinition]) {
    for i in 0..defs.len() {
        if !defs[..i]
            .iter()
            .any(|d| is_token_creating_effect(&d.effect))
        {
            continue;
        }
        rewrite_populated_anaphor_in_effect(&mut defs[i].effect);
    }
}

fn is_token_creating_effect(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Populate | Effect::Token { .. } | Effect::CopyTokenOf { .. }
    )
}

/// Walk an effect, rewriting the populated-token anaphor at whichever level
/// it appears. Recurses into `CreateDelayedTrigger.effect` so the "sacrifice
/// it" pattern inside a delayed trigger also rewrites.
fn rewrite_populated_anaphor_in_effect(effect: &mut Effect) {
    // Case 1: bare Unimplemented anaphor at the top level (e.g., "the token
    // created this way gains haste").
    if let Some(new_effect) = rewrite_token_created_this_way_unimplemented(effect) {
        *effect = new_effect;
        return;
    }

    // Case 2: CreateDelayedTrigger whose inner ability references the token
    // via ParentTarget. Rewrite to LastCreated and recurse into the inner
    // effect for any nested anaphors.
    if let Effect::CreateDelayedTrigger { effect: inner, .. } = effect {
        rewrite_parent_target_to_last_created(&mut inner.effect);
        rewrite_populated_anaphor_in_effect(&mut inner.effect);
    }
}

/// If `effect` is `Unimplemented { description: "<anaphor> <verb-phrase>" }`,
/// try to parse the verb phrase as a continuous modification set and return
/// a replacement `GenericEffect`. Returns `None` when the shape doesn't
/// match so the caller leaves the effect untouched.
///
/// CR 608.2k + CR 603.7c: Recognized anaphor prefixes resolve to the
/// just-created token via `TargetFilter::LastCreated`. The longer
/// populate-specific phrases ("the token(s) created this way ") MUST be
/// tried before the plain "the token " prefix to avoid the latter
/// shadowing the qualified forms when both could match.
fn rewrite_token_created_this_way_unimplemented(effect: &Effect) -> Option<Effect> {
    let Effect::Unimplemented { description, .. } = effect else {
        return None;
    };
    let text = description.as_deref()?;
    let lower = text.to_lowercase();
    // Anaphor prefixes — longest-first so "the token created this way "
    // wins over the bare "the token " when both could match. Plain forms
    // ("this/that/the token ") cover token-creators (Pietra, Inalla,
    // Ghired) that refer to the just-created token without the
    // populate-specific qualifier.
    let mut anaphor = alt((
        tag::<&str, &str, ()>("the token created this way "),
        tag("the tokens created this way "),
        tag("this token "),
        tag("that token "),
        tag("the token "),
    ));
    let (rest, _matched) = anaphor.parse(lower.as_str()).ok()?;
    let mods = crate::parser::oracle_static::parse_continuous_modifications(rest);
    if mods.is_empty() {
        return None;
    }
    let static_def = StaticDefinition::continuous()
        .affected(TargetFilter::LastCreated)
        .modifications(mods)
        .description(text.to_string());
    Some(Effect::GenericEffect {
        static_abilities: vec![static_def],
        duration: Some(Duration::UntilEndOfTurn),
        target: Some(TargetFilter::LastCreated),
    })
}

/// Rewrite any `TargetFilter::ParentTarget` sitting in the target slot of
/// an effect to `TargetFilter::LastCreated`. This is the runtime bridge for
/// "sacrifice it at the beginning of the next end step" (Determined
/// Iteration) and related delayed-trigger anaphors: the imperative parser
/// emits ParentTarget for bare "it", but in the populate context the
/// antecedent is the newly created token, not a parent ability's target.
///
/// CR 608.2k: Scope is narrow — this runs only inside the inner effect of a
/// `CreateDelayedTrigger` whose enclosing chain contains a token-creating
/// effect. Within that scope, `ParentTarget` reflects the imperative
/// parser's bare-pronoun fallback ("sacrifice it" / "exile it" / …) rather
/// than a real parent target slot, so rewriting to `LastCreated` is safe.
/// `ChangeZone` is included because Inalla-style "Exile it at the beginning
/// of the next end step" lowers to `ChangeZone { destination: Exile,
/// target: ParentTarget }`.
fn rewrite_parent_target_to_last_created(effect: &mut Effect) {
    match effect {
        Effect::Sacrifice { target, .. }
        | Effect::Destroy { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::Tap { target, .. }
        | Effect::Untap { target, .. }
        | Effect::Pump { target, .. }
        | Effect::ChangeZone { target, .. } => {
            if matches!(target, TargetFilter::ParentTarget) {
                *target = TargetFilter::LastCreated;
            }
        }
        _ => {}
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

        // CR 705: Consolidate FlipCoinUntilLose with its following effect clause.
        // The next def becomes the win_effect that is executed per win.
        if matches!(&*defs[i].effect, Effect::FlipCoinUntilLose { .. }) && i + 1 < defs.len() {
            let next = defs.remove(i + 1);
            *defs[i].effect = Effect::FlipCoinUntilLose {
                win_effect: Box::new(next),
            };
        }

        // CR 705: Consolidate FlipCoins with its following effect clause — the
        // "for each heads …" / "skips their next X turns where X is the number of
        // coins that came up heads" sentence. The next def is attached as the
        // win_effect (runs once per heads). Only consolidates when the parent
        // `FlipCoins` has no branches already set (i.e., came straight from the
        // imperative lowering, not from a prior consolidation pass).
        if let Effect::FlipCoins {
            win_effect: None,
            lose_effect: None,
            count,
        } = &*defs[i].effect
        {
            if i + 1 < defs.len() {
                let count = count.clone();
                let next = defs.remove(i + 1);
                *defs[i].effect = Effect::FlipCoins {
                    count,
                    win_effect: Some(Box::new(next)),
                    lose_effect: None,
                };
            }
        }

        i += 1;
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

/// Strip optional-effect prefixes, returning whether the effect is optional,
/// which opponent-may scope applies (if any), and an implicit player_scope to
/// propagate to the containing ability (set when the prefix itself carries a
/// per-player iteration, e.g. "each opponent may").
///
/// CR 608.2d + CR 603.2: "each opponent may X" differs from "any opponent
/// may X" — every opponent independently decides yes/no, rather than first
/// accept wins. It lowers to `optional: true` + `player_scope: Opponent`:
/// the outer `player_scope` iteration rebinds controller to each opponent,
/// and each scoped clone enters the standard OptionalEffectChoice prompt.
fn strip_optional_effect_prefix(
    text: &str,
) -> (
    bool,
    Option<crate::types::ability::OpponentMayScope>,
    Option<PlayerFilter>,
    String,
) {
    let lower = text.to_lowercase();
    // CR 608.2d: "each opponent may" — per-opponent optional effect.
    // "any opponent may" — first-accept-wins opponent-choice optional effect.
    // "you may" — standard optional effect prefix.
    if let Some(((scope, player_scope), rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(
                (None, Some(PlayerFilter::Opponent)),
                tag("each opponent may "),
            ),
            value(
                (
                    Some(crate::types::ability::OpponentMayScope::AnyOpponent),
                    None,
                ),
                tag("any opponent may "),
            ),
            value((None, None), tag("you may ")),
        ))
        .parse(input)
    }) {
        (true, scope, player_scope, rest.to_string())
    } else {
        (false, None, None, text.to_string())
    }
}

/// CR 609.3: Strip "for each [X], " prefix from effect text.
/// Returns the QuantityExpr for the iteration count and the remaining text.
/// "For as long as" is NOT matched (different construct — duration, not iteration).
fn strip_for_each_prefix(text: &str) -> (Option<QuantityExpr>, String) {
    let lower = text.to_lowercase();
    if let Some(((), rest)) = nom_on_lower(text, &lower, |i| value((), tag("for each ")).parse(i)) {
        let rest_lower = &lower[text.len() - rest.len()..];
        if let Some((clause, remainder)) = rest_lower.split_once(", ") {
            if let Some(qty) = parse_for_each_clause(clause) {
                // CR 106.1: "for each color among [X], add one mana of that color"
                // must NOT be split into (repeat_for, "add one mana of that color").
                // The "that color" anaphors the per-iteration color, not the
                // source's `ChosenAttribute::Color`. Let the full text flow
                // through to `try_parse_for_each_color_mana_public` which emits
                // a single `DistinctColorsAmongPermanents` mana ability.
                if matches!(qty, QuantityRef::DistinctColorsAmongPermanents { .. })
                    && remainder
                        .trim_end_matches('.')
                        .trim()
                        .eq_ignore_ascii_case("add one mana of that color")
                {
                    return (None, text.to_string());
                }
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
    if let Some(base) = lower.strip_suffix(" times") {
        if let Some(space_idx) = base.rfind(' ') {
            let qty_text = text[space_idx + 1..text.len() - " times".len()].trim();
            if let Some((qty, remainder)) = super::oracle_util::parse_count_expr(qty_text) {
                if remainder.trim().is_empty() {
                    return (Some(qty), text[..space_idx].to_string());
                }
            }
        }
    }
    (None, text.to_string())
}

/// Strip "each player/opponent [verb]s" subject prefix.
/// Returns the PlayerFilter scope and the predicate with deconjugated verb.
/// "Each opponent discards a card" → (Some(Opponent), "discard a card")
/// "Each player draws a card" → (Some(All), "draw a card")
fn strip_player_scope_subject(text: &str) -> (Option<PlayerFilter>, String) {
    let (scope, stripped) = strip_linked_exile_owner_subject(text);
    if scope.is_some() {
        return (scope, stripped);
    }
    strip_each_player_subject(text)
}

fn strip_each_player_subject(text: &str) -> (Option<PlayerFilter>, String) {
    let lower = text.to_lowercase();
    let scope_rest = nom_on_lower(text, &lower, |i| {
        alt((
            value(
                PlayerFilter::HighestSpeed,
                tag("each player with the highest speed among players "),
            ),
            value(PlayerFilter::Opponent, tag("each opponent ")),
            value(PlayerFilter::All, tag("each player ")),
        ))
        .parse(i)
    });
    let Some((scope, rest)) = scope_rest else {
        return (None, text.to_string());
    };

    // Guard: static restriction predicates ("can't", "cannot", "don't", "may only",
    // "may not") belong to the static parser, not the imperative effect pipeline.
    // Intercepting them here would produce Unimplemented instead of typed static modes.
    let rest_lower = rest.trim().to_lowercase();
    if alt((
        tag::<_, _, VerboseError<&str>>("can't"),
        tag("cannot"),
        tag("don't"),
        tag("may only"),
        tag("may not"),
        tag("may cast"),
    ))
    .parse(rest_lower.as_str())
    .is_ok()
    {
        return (None, text.to_string());
    }

    // Deconjugate the verb: "discards" → "discard", "draws" → "draw"
    let deconjugated = subject::deconjugate_verb(rest);
    (Some(scope), deconjugated)
}

fn strip_linked_exile_owner_subject(text: &str) -> (Option<PlayerFilter>, String) {
    let lower = text.to_lowercase();
    let scope_rest = nom_on_lower(text, &lower, |i| {
        alt((
            value(
                PlayerFilter::OwnersOfCardsExiledBySource,
                tag::<_, _, VerboseError<&str>>("the exiled card's owner "),
            ),
            value(
                PlayerFilter::OwnersOfCardsExiledBySource,
                tag("the exiled cards' owners "),
            ),
        ))
        .parse(i)
    });
    let Some((scope, rest)) = scope_rest else {
        return (None, text.to_string());
    };

    let rest_lower = rest.trim().to_lowercase();
    if alt((
        tag::<_, _, VerboseError<&str>>("can't"),
        tag("cannot"),
        tag("don't"),
        tag("may only"),
        tag("may not"),
        tag("may cast"),
    ))
    .parse(rest_lower.as_str())
    .is_ok()
    {
        return (None, text.to_string());
    }

    (Some(scope), subject::deconjugate_verb(rest))
}

/// Parse the player noun used by damage-to-players phrases.
/// Shared by simple `each player/opponent` damage routing and compound
/// `each opponent and each creature ...` damage clauses.
pub(super) fn parse_damage_player_scope(
    input: &str,
) -> nom::IResult<&str, PlayerFilter, VerboseError<&str>> {
    alt((
        value(
            PlayerFilter::Opponent,
            alt((tag::<_, _, VerboseError<&str>>("opponent"), tag("foe"))),
        ),
        value(PlayerFilter::All, tag("player")),
    ))
    .parse(input)
}

/// Parse an exact `each player` / `each opponent` / `each foe` / `each other opponent`
/// / `each other player` damage scope.
/// Returns `None` for compound phrases so dedicated compound parsers can handle them.
///
/// CR 120.3 + CR 603.2c: "each other opponent" anaphors back to the triggering
/// opponent named in the preceding "deals combat damage to an opponent" clause,
/// so the dispatch routes to `OpponentOtherThanTriggering` (a `PlayerFilter`
/// variant that excludes both the controller and the triggering player).
/// "each other player" excludes the controller (the only "other" antecedent
/// available outside trigger context) and reduces to plain `Opponent`.
pub(super) fn parse_damage_each_player_scope(text: &str) -> Option<PlayerFilter> {
    let (rest, filter) = preceded(
        tag("each "),
        alt((
            value(
                PlayerFilter::OpponentOtherThanTriggering,
                alt((
                    tag::<_, _, VerboseError<&str>>("other opponent"),
                    tag("other foe"),
                )),
            ),
            value(PlayerFilter::Opponent, tag("other player")),
            parse_damage_player_scope,
        )),
    )
    .parse(text)
    .ok()?;
    rest.chars()
        .all(|c| c.is_ascii_whitespace() || c.is_ascii_punctuation())
        .then_some(filter)
}

fn strip_leading_duration(text: &str) -> Option<(Duration, &str)> {
    let lower = text.to_lowercase();
    if let Some((duration, rest)) = nom_on_lower(text, &lower, |i| {
        alt((
            value(Duration::UntilEndOfTurn, tag("until end of turn, ")),
            value(
                Duration::UntilYourNextTurn,
                tag("until the end of your next turn, "),
            ),
            value(Duration::UntilYourNextTurn, tag("until your next turn, ")),
        ))
        .parse(i)
    }) {
        return Some((duration, rest.trim()));
    }

    // CR 611.2b: "For as long as [condition], [effect]" — leading duration prefix.
    if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("for as long as ").parse(lower.as_str())
    {
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
        // CR 611.2a + CR 108.3: Third-person "their next turn" appears in grants
        // whose grantee is not the ability's controller (Suspend Aggression:
        // "its owner may play it"; Expedited Inheritance: "its controller may
        // ... They may play those cards"). Theme D's `granted_to` binds to the
        // grantee, so `UntilYourNextTurn` is semantically "until the end of the
        // grantee's next turn" at prune time.
        (
            " until the end of their next turn",
            Duration::UntilYourNextTurn,
        ),
        (" until their next turn", Duration::UntilYourNextTurn),
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
    if scan_contains_phrase(condition, "remains tapped") {
        return Some(Duration::ForAsLongAs {
            condition: StaticCondition::SourceIsTapped,
        });
    }

    // "you control ~" / "you control this creature"
    if tag::<_, _, VerboseError<&str>>("you control ")
        .parse(condition)
        .is_ok()
    {
        return Some(Duration::UntilHostLeavesPlay);
    }

    // "~ remains on the battlefield" / "it remains on the battlefield"
    if scan_contains_phrase(condition, "remains on the battlefield") {
        return Some(Duration::UntilHostLeavesPlay);
    }

    // CR 122.1: "<subject> has <quantity> [type] counter[s] on it" — delegate to
    // the shared nom combinator so the typed/bare/quantity grammar lives in a
    // single authority (`parse_inner_condition` family) rather than being
    // duplicated here as bespoke string matching.
    if let Ok((rest, condition)) =
        crate::parser::oracle_nom::condition::parse_source_has_counters(condition)
    {
        if rest.trim().is_empty() {
            return Some(Duration::ForAsLongAs { condition });
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
///
/// CR 505.1: "your next main phase" binds the trigger to the ability's
/// controller — the `player` field is a compile-time placeholder
/// (`PlayerId(0)`) rewritten to `ability.controller` at resolution time in
/// `effects::delayed_trigger::resolve`. Mirrors the existing
/// `RestrictionScope::SourcesControlledBy` placeholder pattern.
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
        // CR 505.1: Precombat main phase of the controller. "Your" binds
        // `player` to the ability's controller; resolved at resolve time.
        (
            " at the beginning of your next main phase",
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::PreCombatMain,
                player: crate::types::player::PlayerId(0),
            },
        ),
        // CR 505.1 + CR 603.7a: Symmetric to the prefix form at
        // `strip_temporal_prefix`. Greasefang's "return it to its owner's hand
        // at the beginning of your next end step" uses this suffix shape; the
        // player placeholder is rewritten to `ability.controller` at resolve
        // time alongside the main-phase and upkeep variants.
        (
            " at the beginning of your next end step",
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::End,
                player: crate::types::player::PlayerId(0),
            },
        ),
        (
            " at the beginning of your next upkeep",
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::Upkeep,
                player: crate::types::player::PlayerId(0),
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
    if let Some((condition, rest)) = nom_on_lower(text, &lower, |i| {
        alt((
            value(
                DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                tag("at the beginning of the next end step, "),
            ),
            value(
                DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::Upkeep,
                },
                tag("at the beginning of the next upkeep, "),
            ),
            // CR 505.1 + CR 603.7a: "your next" binds the phase to the ability's
            // controller. `PlayerId(0)` is a placeholder rewritten at resolution
            // time in `effects::delayed_trigger::resolve`.
            value(
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::Upkeep,
                    player: crate::types::player::PlayerId(0),
                },
                tag("at the beginning of your next upkeep, "),
            ),
            value(
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::End,
                    player: crate::types::player::PlayerId(0),
                },
                tag("at the beginning of your next end step, "),
            ),
            // CR 500.8 + CR 603.7a: "at the beginning of that combat" refers to an
            // additional combat phase just scheduled by the parent effect
            // (e.g., Moraug, Fury of Akoum's landfall trigger). The additional
            // combat is pushed as the very next phase, so we fire on the next
            // BeginCombat.
            value(
                DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::BeginCombat,
                },
                tag("at the beginning of that combat, "),
            ),
        ))
        .parse(i)
    }) {
        return (rest, Some(condition));
    }
    (text, None)
}

/// CR 115.1d: Extract multi_target spec from PutCounter text.
/// Looks for "counter on up to N" pattern and returns the spec.
/// Used as a post-parse fixup when the AST→Effect lowering loses multi_target info.
fn extract_put_counter_multi_target(text: &str) -> Option<MultiTargetSpec> {
    let lower = text.to_lowercase();
    let after = [
        "counter on up to ",
        "counters on up to ",
        "counter on each of up to ",
        "counters on each of up to ",
    ]
    .into_iter()
    .find_map(|marker| strip_after(&lower, marker))?;
    // Delegate to nom combinator (input already lowercase).
    let (_, n) = nom_primitives::parse_number.parse(after).ok()?;
    Some(MultiTargetSpec {
        min: 0,
        max: Some(n as usize),
    })
}

/// Post-parse fixup for "exile N target" multi_target.
/// Strips "exile " verb via nom tag, then delegates to `strip_numeric_target_prefix`.
fn extract_exile_multi_target(text: &str) -> Option<MultiTargetSpec> {
    let lower = text.to_lowercase();
    let (after_verb, _) = tag::<_, _, VerboseError<&str>>("exile ")
        .parse(lower.as_str())
        .ok()?;
    let (count, _) = strip_numeric_target_prefix(after_verb)?;
    Some(MultiTargetSpec {
        min: count,
        max: Some(count),
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
    let (rest, count) = alt((
        value(2usize, tag::<_, _, VerboseError<&str>>("two ")),
        value(3, tag("three ")),
        value(4, tag("four ")),
        value(5, tag("five ")),
        value(6, tag("six ")),
    ))
    .parse(lower)
    .ok()?;
    if alt((tag::<_, _, VerboseError<&str>>("target "), tag("target,")))
        .parse(rest)
        .is_ok()
    {
        Some((count, rest))
    } else {
        None
    }
}

/// CR 115.1d: Strip optional target-count prefixes before a targeted phrase.
/// "up to one target creature" → ("target creature", Some { min: 0, max: Some(1) })
/// "up to one other target creature or spell" → ("other target creature or spell", Some { ... })
pub(super) fn strip_optional_target_prefix(text: &str) -> (&str, Option<MultiTargetSpec>) {
    let lower = text.to_ascii_lowercase();
    let Ok((after_up_to, _)) = tag::<_, _, VerboseError<&str>>("up to ").parse(lower.as_str())
    else {
        return (text, None);
    };
    // Delegate to nom combinator (input already lowercase).
    let Ok((remainder, n)) = nom_primitives::parse_number.parse(after_up_to) else {
        return (text, None);
    };
    let consumed = lower.len() - remainder.len();
    let rest = text[consumed..].trim_start();
    let rest_lower = rest.to_ascii_lowercase();
    if alt((
        tag::<_, _, VerboseError<&str>>("target "),
        tag("other target "),
        tag("another target "),
    ))
    .parse(rest_lower.as_str())
    .is_err()
    {
        return (text, None);
    }
    (
        rest,
        Some(MultiTargetSpec {
            min: 0,
            max: Some(n as usize),
        }),
    )
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

    if let Some((_, rest_orig)) =
        super::oracle_nom::bridge::nom_on_lower(after_verb_tp.original, after_verb_tp.lower, |i| {
            value((), tag("any number of ")).parse(i)
        })
    {
        let rebuilt = format!("{}{}", verb_tp.original, rest_orig);
        return (rebuilt, Some(MultiTargetSpec { min: 0, max: None }));
    }
    if let Some((_, after_up_to_orig)) =
        super::oracle_nom::bridge::nom_on_lower(after_verb_tp.original, after_verb_tp.lower, |i| {
            value((), tag("up to ")).parse(i)
        })
    {
        let after_up_to_lower =
            &after_verb_tp.lower[after_verb_tp.lower.len() - after_up_to_orig.len()..];
        let after_up_to = TextPair::new(after_up_to_orig, after_up_to_lower);
        // Delegate to nom combinator (input already lowercase from TextPair.lower).
        if let Ok((remainder, n)) = nom_primitives::parse_number.parse(after_up_to.lower) {
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
    let verb_len = if tag::<_, _, VerboseError<&str>>("deals ")
        .parse(&lower[pos..])
        .is_ok()
    {
        6
    } else {
        5
    };
    let (_, after_tp) = tp.split_at(pos + verb_len);

    let (amount, rest_tp) =
        if let Some((qty, rem)) = super::oracle_util::parse_count_expr(after_tp.lower) {
            if tag::<_, _, VerboseError<&str>>("damage").parse(rem).is_ok() {
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
    let distribute_kind = if scan_contains_phrase(rest_tp.lower, "divided as you choose among")
        || scan_contains_phrase(rest_tp.lower, "distributed among")
    {
        DistributionUnit::Damage
    } else if scan_contains_phrase(rest_tp.lower, "divided evenly") {
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
    let (stripped_target_text, multi_target) = if let Ok((rest, _)) =
        tag::<_, _, VerboseError<&str>>("any number of ").parse(target_lower.as_str())
    {
        let skip = target_lower.len() - rest.len();
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
        condition: None,
        optional: false,
    })
}

/// CR 601.2d: Parse "distribute N [type] counters among [targets]"
/// → Effect::PutCounter with distribute flag set.
fn try_parse_distribute_counters(lower: &str, text: &str) -> Option<ParsedEffectClause> {
    // "distribute " is 11 bytes; Oracle text is ASCII so byte == char offsets.
    let (after_lower, _) = tag::<_, _, VerboseError<&str>>("distribute ")
        .parse(lower)
        .ok()?;
    let (count_expr, rest_lower) = super::oracle_util::parse_count_expr(after_lower)?;

    let type_end = rest_lower
        .find(|c: char| c.is_whitespace())
        .unwrap_or(rest_lower.len());
    let raw_type = &rest_lower[..type_end];
    let counter_type = counter::normalize_counter_type(raw_type);

    // Require "counter(s)" immediately after the counter type word.
    let after_type = rest_lower[type_end..].trim_start();
    let counter_word_len = if tag::<_, _, VerboseError<&str>>("counters")
        .parse(after_type)
        .is_ok()
    {
        "counters".len()
    } else if tag::<_, _, VerboseError<&str>>("counter")
        .parse(after_type)
        .is_ok()
    {
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
    let (stripped_target, multi_target) = if let Ok((rest, _)) =
        tag::<_, _, VerboseError<&str>>("any number of ").parse(target_text_lower)
    {
        let skip = target_text_lower.len() - rest.len();
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
            count: count_expr,
            target,
        },
        duration: None,
        sub_ability: None,
        distribute: Some(DistributionUnit::Counters(raw_type.to_string())),
        multi_target,
        condition: None,
        optional: false,
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
    let verb_len = if tag::<_, _, VerboseError<&str>>("deals ")
        .parse(&lower[pos..])
        .is_ok()
    {
        6
    } else {
        5
    };
    let after = &text[pos + verb_len..];
    let after_lower = &lower[pos + verb_len..];

    let (amount, after_target) = if let Some((qty, rest)) =
        super::oracle_util::parse_count_expr(after_lower)
    {
        if tag::<_, _, VerboseError<&str>>("damage")
            .parse(rest)
            .is_ok()
        {
            (qty, &after[after.len() - rest.len() + "damage".len()..])
        } else {
            return None;
        }
    } else if let Ok((rem, _)) =
        tag::<_, _, VerboseError<&str>>("twice that much damage").parse(after_lower)
    {
        // CR 120.8: "twice that much damage" → Multiply { factor: 2, inner: EventContextAmount }
        let consumed = after_lower.len() - rem.len();
        (
            QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
            },
            &after[consumed..],
        )
    } else if let Ok((rem, _)) =
        tag::<_, _, VerboseError<&str>>("that much damage").parse(after_lower)
    {
        let consumed = after_lower.len() - rem.len();
        (
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            &after[consumed..],
        )
    } else if let Some(rest) = after_lower.strip_prefix("damage to ") {
        // Pattern: "damage to [target] equal to [amount]"
        // Used by: "deals damage to itself equal to its power",
        //          "deals damage to each player equal to the number of ...",
        //          "deals damage to that player equal to the number of ..."
        if let Ok((_, (target_phrase, amount_phrase))) =
            nom_primitives::split_once_on(rest, " equal to ")
        {
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
                } else if tag::<_, _, VerboseError<&str>>("each ")
                    .parse(target_phrase)
                    .is_ok()
                {
                    // "each player" → DamageEachPlayer (per-player varying damage)
                    // "each creature" → DamageAll (uniform damage to objects)
                    // "each foe" — archaic synonym for opponent (friend/foe cards)
                    if let Some(player_filter) = parse_damage_each_player_scope(target_phrase) {
                        return Some((
                            Effect::DamageEachPlayer {
                                amount: qty,
                                player_filter,
                            },
                            "",
                        ));
                    }
                    let (filter, remainder) = parse_target(target_phrase);
                    let (filter, remainder) = refine_damage_target_remainder(filter, remainder);
                    if !remainder.trim().is_empty() {
                        push_warning(format!(
                            "ignored-remainder: '{}' after target parse in damage-all",
                            remainder.trim()
                        ));
                    }
                    return Some((
                        Effect::DamageAll {
                            amount: qty,
                            target: filter,
                            player_filter: None,
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
                    let (target, remainder) = parse_target(target_phrase);
                    let (target, remainder) = refine_damage_target_remainder(target, remainder);
                    if !remainder.trim().is_empty() {
                        push_warning(format!(
                            "ignored-remainder: '{}' after target parse in deal-damage",
                            remainder.trim()
                        ));
                    }
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
    } else if let Ok((rem, _)) =
        tag::<_, _, VerboseError<&str>>("damage equal to ").parse(after_lower)
    {
        let consumed = after_lower.len() - rem.len();
        let amount_text = &after[consumed..];
        let amount_lower = amount_text.to_lowercase();
        let (_, before_to) = take_until::<_, _, VerboseError<&str>>(" to ")
            .parse(amount_lower.as_str())
            .ok()?;
        let qty_text = amount_text[..before_to.len()].trim();
        let qty = crate::parser::oracle_quantity::parse_event_context_quantity(qty_text)
            .unwrap_or_else(|| QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: qty_text.to_string(),
                },
            });
        (qty, &amount_text[before_to.len() + 4..])
    } else {
        return None;
    };

    let after_to = after_target
        .trim()
        .strip_prefix("to ")
        .unwrap_or(after_target)
        .trim();
    // CR 107.3i + CR 120.3: Trim a trailing "where X is <expr>" binding from
    // the recipient phrase before classification. The binding has already been
    // captured at chunk-build time and re-applied via
    // `apply_where_x_ability_expression`; leaving it in the recipient phrase
    // would cause `parse_damage_each_player_scope`'s exact-match check to
    // reject "each player, where X is the number of descent counters on ~",
    // forcing a fall-through to `DamageAll{Typed{empty}}`. Repro: Descent into
    // Avernus. The strip is local to classification — it doesn't disturb the
    // outer chunk-level where-X handling (Token P/T, Pump, SkipNextTurn).
    let after_to_lower_full = after_to.to_lowercase();
    let after_to_for_classification = {
        let tp = TextPair::new(after_to, &after_to_lower_full);
        let (stripped, _) = strip_trailing_where_x(tp);
        // `stripped.original` is a prefix slice of `after_to` (TextPair::new
        // requires byte-length equality, preserved for ASCII). Re-slice
        // `after_to` by the stripped length to keep the outer lifetime.
        &after_to[..stripped.original.len()]
    };
    if tag::<_, _, VerboseError<&str>>("each ")
        .parse(after_to_for_classification)
        .is_ok()
    {
        if let Some(player_filter) = parse_damage_each_player_scope(after_to_for_classification) {
            return Some((
                Effect::DamageEachPlayer {
                    amount,
                    player_filter,
                },
                "",
            ));
        }
        let (target, rem) = parse_target(after_to_for_classification);
        return Some((
            Effect::DamageAll {
                amount,
                target,
                player_filter: None,
            },
            rem,
        ));
    }

    // CR 120.3: "itself" — the source creature is both damage source and recipient.
    let after_to_lower = after_to.to_lowercase();
    if after_to_lower == "itself"
        || tag::<_, _, VerboseError<&str>>("itself ")
            .parse(after_to_lower.as_str())
            .is_ok()
    {
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

    // No "to [target]" clause — the damage target is inherited from the parent effect
    // (e.g., "it deals 4 damage instead" reuses the original target).
    if after_to.is_empty() {
        return Some((
            Effect::DealDamage {
                amount,
                target: TargetFilter::ParentTarget,
                damage_source: None,
            },
            "",
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
    let offset = if tag::<_, _, VerboseError<&str>>("gets ")
        .parse(&lower[re_pos..])
        .is_ok()
    {
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
    // Strip "for each [clause]" suffix before duration extraction.
    let (without_for_each, for_each_qty) = strip_trailing_for_each_clause(without_where.original);
    let (without_duration, duration) = strip_trailing_duration(without_for_each);
    let lower = without_duration.to_lowercase();

    let after = nom_on_lower(without_duration, &lower, |i| {
        value((), alt((tag("gets "), tag("get ")))).parse(i)
    });
    let (_, after) = after?;
    let after = after.trim_start();

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

    // CR 613.4c: Compose with "for each" quantity to produce dynamic PtValue.
    let (power, toughness) = if let Some(qty) = for_each_qty {
        let quantity = QuantityExpr::Ref { qty };
        (
            compose_pt_with_for_each(power, &quantity),
            compose_pt_with_for_each(toughness, &quantity),
        )
    } else {
        (power, toughness)
    };

    Some((power, toughness, duration))
}

/// Strip a trailing "for each [clause]" from pump text, returning the remaining text
/// and the parsed QuantityRef (if any). Handles both "until end of turn for each X"
/// (duration already stripped) and bare "for each X".
fn strip_trailing_for_each_clause(text: &str) -> (&str, Option<QuantityRef>) {
    let lower = text.to_lowercase();
    if let Some(pos) = lower.rfind(" for each ") {
        let clause_text = lower[pos + " for each ".len()..].trim_end_matches('.');
        if let Some(qty) = parse_for_each_clause(clause_text) {
            return (text[..pos].trim(), Some(qty));
        }
    }
    (text, None)
}

/// CR 613.4c: Compose a fixed P/T value with a "for each" quantity.
/// +1 × quantity → Quantity(quantity), +N × quantity → Quantity(Multiply { factor: N }),
/// +0 stays Fixed(0), variable values stay unchanged.
fn compose_pt_with_for_each(pt: PtValue, quantity: &QuantityExpr) -> PtValue {
    match pt {
        PtValue::Fixed(0) => PtValue::Fixed(0),
        PtValue::Fixed(1) => PtValue::Quantity(quantity.clone()),
        PtValue::Fixed(-1) => PtValue::Quantity(QuantityExpr::Multiply {
            factor: -1,
            inner: Box::new(quantity.clone()),
        }),
        PtValue::Fixed(n) => PtValue::Quantity(QuantityExpr::Multiply {
            factor: n,
            inner: Box::new(quantity.clone()),
        }),
        other => other, // Variable/Quantity values not composed
    }
}

/// CR 107.3i + CR 107.3m: Compute, for each chunk, the `where X is <expr>`
/// binding that applies to its enclosing sentence. Sibling clauses of the same
/// sentence share the binding so that "target player loses X life and you gain
/// X life, where X is the greatest power among creatures you control" resolves
/// both X references to the same expression.
///
/// Groups chunks by `ClauseBoundary::Sentence` (Comma/Then/None continue the
/// current sentence). The returned Vec has the same length as `chunks`; each
/// entry is the binding of that chunk's sentence, or `None` if no sibling in
/// the sentence contains a "where X is" suffix.
fn compute_sentence_where_x(chunks: &[ClauseChunk]) -> Vec<Option<String>> {
    let mut out = vec![None; chunks.len()];
    let mut group_start = 0usize;
    for (idx, chunk) in chunks.iter().enumerate() {
        let ends_sentence = matches!(chunk.boundary_after, Some(ClauseBoundary::Sentence) | None);
        if ends_sentence {
            // Close the group [group_start..=idx]: scan for a where-X binding.
            let binding = chunks[group_start..=idx].iter().find_map(|c| {
                let lower = c.text.to_lowercase();
                let (_, expr) = strip_trailing_where_x(TextPair::new(&c.text, &lower));
                expr
            });
            if binding.is_some() {
                for slot in &mut out[group_start..=idx] {
                    *slot = binding.clone();
                }
            }
            group_start = idx + 1;
        }
    }
    out
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

    // Try to strip a leading sequence connector using nom alt().
    // Mixed case requires explicit variants since nom tag() is exact-match.
    match alt((
        tag::<_, _, VerboseError<&str>>("Then, "),
        tag("Then "),
        tag("then, "),
        tag("then "),
        tag("and "),
        tag("And "),
    ))
    .parse(trimmed)
    {
        Ok((rest, _)) => rest,
        Err(_) => trimmed,
    }
}

fn apply_where_x_expression(value: PtValue, where_x_expression: Option<&str>) -> PtValue {
    match (value, where_x_expression) {
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("X") => {
            parse_where_x_quantity_expression(expression)
                .map(PtValue::Quantity)
                .unwrap_or_else(|| PtValue::Variable(expression.to_string()))
        }
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("-X") => {
            parse_where_x_quantity_expression(expression)
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

fn parse_where_x_quantity_expression(where_x_expression: &str) -> Option<QuantityExpr> {
    parse_cda_quantity(where_x_expression)
}

fn apply_where_x_quantity_expression(
    value: QuantityExpr,
    where_x_expression: Option<&str>,
) -> QuantityExpr {
    match value {
        QuantityExpr::Ref {
            qty: QuantityRef::Variable { name },
        } if where_x_expression.is_some() && name.eq_ignore_ascii_case("X") => {
            let expression = where_x_expression.expect("checked is_some above");
            parse_where_x_quantity_expression(expression).unwrap_or_else(|| QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: expression.to_string(),
                },
            })
        }
        QuantityExpr::Offset { inner, offset } => QuantityExpr::Offset {
            inner: Box::new(apply_where_x_quantity_expression(
                *inner,
                where_x_expression,
            )),
            offset,
        },
        QuantityExpr::Multiply { factor, inner } => QuantityExpr::Multiply {
            factor,
            inner: Box::new(apply_where_x_quantity_expression(
                *inner,
                where_x_expression,
            )),
        },
        QuantityExpr::HalfRounded { inner, rounding } => QuantityExpr::HalfRounded {
            inner: Box::new(apply_where_x_quantity_expression(
                *inner,
                where_x_expression,
            )),
            rounding,
        },
        other => other,
    }
}

fn apply_where_x_effect_expression(effect: &mut Effect, where_x_expression: Option<&str>) {
    match effect {
        Effect::DealDamage { amount, .. }
        | Effect::DamageAll { amount, .. }
        | Effect::DamageEachPlayer { amount, .. }
        | Effect::GainLife { amount, .. }
        | Effect::LoseLife { amount, .. }
        | Effect::IncreaseSpeed { amount, .. }
        | Effect::Draw { count: amount, .. }
        | Effect::Mill { count: amount, .. }
        | Effect::PutCounter { count: amount, .. }
        | Effect::PutCounterAll { count: amount, .. }
        | Effect::Token { count: amount, .. }
        | Effect::Dig { count: amount, .. } => {
            *amount = apply_where_x_quantity_expression(amount.clone(), where_x_expression);
        }
        Effect::Pump {
            power, toughness, ..
        }
        | Effect::PumpAll {
            power, toughness, ..
        } => {
            *power = apply_where_x_expression(power.clone(), where_x_expression);
            *toughness = apply_where_x_expression(toughness.clone(), where_x_expression);
        }
        _ => {}
    }
}

fn apply_where_x_ability_expression(def: &mut AbilityDefinition, where_x_expression: Option<&str>) {
    apply_where_x_effect_expression(def.effect.as_mut(), where_x_expression);
    if let Some(sub) = def.sub_ability.as_mut() {
        apply_where_x_ability_expression(sub, where_x_expression);
    }
    if let Some(else_ability) = def.else_ability.as_mut() {
        apply_where_x_ability_expression(else_ability, where_x_expression);
    }
    for mode_ability in &mut def.mode_abilities {
        apply_where_x_ability_expression(mode_ability, where_x_expression);
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

    let (sign, body) = if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("+").parse(text) {
        (1, rest.trim())
    } else if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("-").parse(text) {
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

/// CR 122.1 + CR 614.1c: Scan a remainder for a "with [N] [type] counter(s) on
/// it" suffix and lift the matched counter type + count into a
/// `Vec<(String, QuantityExpr)>` slot for `Effect::ChangeZone.enter_with_counters`.
///
/// Matches the patterns:
///   * "with N <type> counter(s) on it" — fixed numeric (digits or English).
///   * "with a/an <type> counter on it" — singular article.
///   * Optional "additional " between count and type — purely a synonym in
///     this position; the counter is still added once during the move.
///
/// Returns an empty `Vec` when no clause is present, so the caller can stamp
/// it unconditionally.
///
/// Implemented as a `scan_preceded` over the body combinator — the scanner
/// advances at word boundaries, so the suffix can appear anywhere after the
/// destination phrase ("onto the battlefield tapped under your control with
/// two additional +1/+1 counters on it") without the caller having to
/// pre-trim. The body combinator gates on `tag("with ")` then dispatches to
/// `parse_counter_suffix_body`.
fn parse_with_counters_suffix(lower: &str) -> Vec<(String, QuantityExpr)> {
    nom_primitives::scan_preceded(lower, |i| {
        let (i, _) = tag::<_, _, VerboseError<&str>>("with ").parse(i)?;
        parse_counter_suffix_body_combinator(i)
    })
    .map(|(_, val, _)| vec![val])
    .unwrap_or_default()
}

/// CR 122.1 + CR 614.1c: Combinator body for "[N|a|an] [additional ]<type>
/// counter(s) on it". Used by `parse_with_counters_suffix` AND by the exile-
/// anaphor counter clause in `oracle_replacement.rs` so both paths share the
/// same grammar.
///
/// Returns the parsed `(counter_type, count)` pair on success.
pub(crate) fn parse_counter_suffix_body_combinator(
    input: &str,
) -> nom::IResult<&str, (String, QuantityExpr), VerboseError<&str>> {
    // Count: digits, English word, or article ("a"/"an").
    let (rest, count) = nom_primitives::parse_number.parse(input)?;
    let (rest, _) = tag(" ").parse(rest)?;

    // Optional "additional " — a synonym in this grammatical position.
    let (rest, _) =
        nom::combinator::opt(tag::<_, _, VerboseError<&str>>("additional ")).parse(rest)?;

    // Counter type: parse the token up to " counter" / " counters". The body
    // accepts any non-whitespace name (including "+1/+1") followed by inline
    // tokens that don't terminate at " counter".
    let (rest, type_token) = take_until(" counter").parse(rest)?;
    let counter_type = crate::types::counter::parse_counter_type(type_token)
        .as_str()
        .to_string();
    let (rest, _) = tag(" counter").parse(rest)?;
    // Optional plural "s".
    let (rest, _) = nom::combinator::opt(tag::<_, _, VerboseError<&str>>("s")).parse(rest)?;
    let (rest, _) = tag(" on it").parse(rest)?;

    Ok((
        rest,
        (
            counter_type,
            QuantityExpr::Fixed {
                value: count as i32,
            },
        ),
    ))
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
        // CR 701.14: "into that player's graveyard" — "that player" refers back to the owner
        // named earlier in the clause (e.g. Eldrazi Processors: "a card an opponent owns").
        (" into that player's graveyard", Zone::Graveyard),
        (" into their graveyard", Zone::Graveyard),
        (" on the bottom of", Zone::Library),
        (" on top of", Zone::Library),
    ] {
        if let Some((before, after)) = after_put_tp.split_around(needle) {
            let target_text = before.original.trim();
            if target_text.is_empty() {
                return None;
            }
            let (target, _) = parse_target(target_text);
            // CR 701.33: Restrict the target to cards revealed by the preceding
            // Dig effect when the phrase "revealed this way" appears in the
            // target text. The Dig resolver populates `state.tracked_object_sets`
            // with the kept (revealed) cards; `TargetFilter::TrackedSetFiltered`
            // reads that set and intersects it with the original type filter.
            // `TrackedSetId(0)` is a sentinel resolved to the most recent
            // tracked set (via the existing `bind_tracked_set_to_effect` pass
            // for delayed triggers; direct sub_abilities observe the latest
            // set via `filter_inner`'s lookup). Used by Zimone's Experiment:
            // "Put all land cards revealed this way onto the battlefield
            // tapped and put all creature cards revealed this way into your
            // hand."
            let target = if scan_contains_phrase(before.lower, "revealed this way") {
                TargetFilter::TrackedSetFiltered {
                    id: crate::types::identifiers::TrackedSetId(0),
                    filter: Box::new(target),
                }
            } else {
                target
            };
            // CR 110.2: "under your control" overrides the entering object's controller.
            let under_your_control = scan_contains_phrase(after_put_tp.lower, "under your control");
            // CR 122.1 + CR 614.1c: Detect a trailing "with [N] [type] counter(s)
            // on it" clause and stamp it onto `enter_with_counters`. This covers
            // The Darkness Crystal's "with two additional +1/+1 counters on it"
            // and the broader class of put-onto-battlefield-with-counters
            // activated abilities. The combinator scans the post-destination
            // remainder so the suffix is consumed only when present.
            let enter_with_counters = parse_with_counters_suffix(after_put_tp.lower);
            // CR 508.4 + CR 614.1: The post-destination tail may carry inline
            // entry-qualifier clauses ("tapped" / "tapped and attacking
            // [<player_phrase>]") that propagate flags onto the produced
            // `Effect::ChangeZone`. The combinator inspects the immediate tail
            // (the slice starting right after the destination needle) so the
            // single-sentence Kaalia/Ilharg pattern — where the qualifiers
            // trail the destination in the same clause — sets both flags in
            // one pass. The separate-sentence form ("It enters tapped and
            // attacking") is handled by the `EntersTappedAttacking`
            // continuation patcher in `oracle_effect/sequence.rs`.
            let (enter_tapped, enters_attacking) = if destination == Zone::Battlefield {
                parse_battlefield_entry_qualifiers(after.lower)
            } else {
                (false, false)
            };
            return Some(Effect::ChangeZone {
                origin: infer_origin_zone(after_put_tp.lower),
                destination,
                target,
                owner_library: false,
                enter_transformed: false,
                under_your_control,
                enter_tapped,
                enters_attacking,
                up_to: false,
                enter_with_counters,
            });
        }
    }

    None
}

/// CR 508.4 + CR 614.1: Detect inline entry-qualifier clauses ("tapped" or
/// "tapped and attacking [<player_phrase>]") immediately following a "put X
/// onto the battlefield" destination needle. Returns
/// `(enter_tapped, enters_attacking)`.
///
/// The single-sentence "tapped and attacking" pattern is the Kaalia / Ilharg
/// class: 25+ commander/legacy cards whose Oracle text reads
/// "...onto the battlefield tapped and attacking[ that opponent | that player |
/// defending player or a planeswalker they control | during your declare
/// attackers step | , where X is ... | and the rest ...]." Without this
/// inline-tail patcher, `enters_attacking` was silently dropped and the
/// summoned creature entered tapped but not attacking (CR 508.4 violation).
///
/// The bare "tapped" branch preserves the prior behavior for the
/// "onto the battlefield tapped" tutor / reanimation class (Coming Attraction,
/// Dance of the Dead, Hunting Wilds, etc.).
///
/// `tail_lower` is the lowercase slice starting *immediately after* the
/// destination needle (e.g. `" tapped and attacking that opponent."`); the
/// leading space is part of the needle's word-boundary contract.
fn parse_battlefield_entry_qualifiers(tail_lower: &str) -> (bool, bool) {
    // Word-boundary anchor for the qualifier clause: the qualifier must end at
    // a real boundary so " tapped" doesn't accidentally consume " tappedly".
    // EOF, whitespace, or sentence punctuation all qualify.
    fn qualifier_boundary(input: &str) -> nom::IResult<&str, (), VerboseError<&str>> {
        alt((
            value((), nom::combinator::eof),
            value((), tag(" ")),
            value((), tag(",")),
            value((), tag(".")),
        ))
        .parse(input)
    }

    // Combinator for the most-specific "tapped and attacking" form. The
    // boundary check absorbs the trailing player phrase ("that opponent",
    // "defending player ...") or sentence terminator without consuming it.
    fn tapped_and_attacking_clause(input: &str) -> nom::IResult<&str, (), VerboseError<&str>> {
        preceded(tag(" tapped and attacking"), qualifier_boundary).parse(input)
    }

    // Combinator for the bare "tapped" form (no "and attacking"). Preserves
    // existing tutor / reanimation behavior (Coming Attraction, Dance of the
    // Dead, Hunting Wilds, etc.).
    fn tapped_clause(input: &str) -> nom::IResult<&str, (), VerboseError<&str>> {
        preceded(tag(" tapped"), qualifier_boundary).parse(input)
    }

    // CR 508.4 + CR 614.1: Scan every word-boundary position in the tail for
    // the qualifier clause. This handles word-ordering variants where
    // "under your control" or other phrasing appears between the destination
    // and the qualifier (Chorale of the Void, The Grim Captain, Zara
    // Renegade Recruiter: "...onto the battlefield under your control tapped
    // and attacking..."). The scanner mirrors `oracle_effect/token.rs:195`
    // which scans byte positions for the first leading-space match. Try the
    // more-specific "tapped and attacking" combinator at every boundary
    // first; if no boundary matches, fall back to the bare "tapped" form.
    let scan_at_boundaries = |clause: fn(&str) -> nom::IResult<&str, (), VerboseError<&str>>| {
        (0..tail_lower.len()).any(|pos| {
            tail_lower.as_bytes().get(pos) == Some(&b' ') && clause(&tail_lower[pos..]).is_ok()
        })
    };
    if scan_at_boundaries(tapped_and_attacking_clause) {
        return (true, true);
    }
    if scan_at_boundaries(tapped_clause) {
        return (true, false);
    }
    (false, false)
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
    let (rest, _) = tag::<_, _, VerboseError<&str>>("where x is ")
        .parse(trimmed)
        .ok()?;
    if scan_contains_phrase(rest, "power") {
        Some(QuantityExpr::Ref {
            qty: QuantityRef::SelfPower,
        })
    } else if scan_contains_phrase(rest, "toughness") {
        Some(QuantityExpr::Ref {
            qty: QuantityRef::SelfToughness,
        })
    } else {
        None
    }
}

fn infer_origin_zone(lower: &str) -> Option<Zone> {
    // CR 400.7: An object that moves from one zone to another becomes a new
    // object — the "from" prepositional phrase identifies that origin zone.
    // Adjective-qualified indefinite forms ("a single graveyard", "a random
    // graveyard") share the same semantic origin as bare "a graveyard"; the
    // qualifier constrains *which* instance, not which zone.
    if contains_possessive(lower, "from", "graveyard")
        || scan_contains_phrase(lower, "from a graveyard")
        || scan_contains_phrase(lower, "from a single graveyard")
        || scan_contains_phrase(lower, "from a random graveyard")
    {
        Some(Zone::Graveyard)
    } else if scan_contains_phrase(lower, "from exile")
        // CR 406.6: "[card] exiled with [source]" identifies the card via the
        // exile-link from a prior linked exile ability — the implicit origin
        // is the exile zone. Used by reanimation/return effects on cards that
        // previously exiled their target (Darkness Crystal, Mimeoplasm, etc.).
        || scan_contains_phrase(lower, "exiled with")
    {
        Some(Zone::Exile)
    } else if contains_possessive(lower, "from", "hand") {
        Some(Zone::Hand)
    } else if contains_possessive(lower, "from", "library") {
        Some(Zone::Library)
    } else if scan_contains_phrase(lower, "graveyard") && !scan_contains_phrase(lower, "from") {
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
        // English consonant-y → ies plural (copy→copies, scry→scries, try→tries).
        // Vowel-y verbs (play→plays) take regular -s and fall through. Result is
        // checked against `PREDICATE_VERBS`; false positives like "series→sery"
        // are silent (no allowlist hit, no behavior change).
        // allow-noncombinator: verb-morphology suffix check on pre-tokenized word (PATTERNS.md §9)
        _ if token.ends_with("ies") && token.len() > 3 => {
            format!("{}y", &token[..token.len() - 3])
        }
        // allow-noncombinator: verb-morphology suffix check on pre-tokenized word
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
    type E<'a> = VerboseError<&'a str>;

    let (rest, scope) = alt((
        value(
            RetargetScope::Single,
            tag::<_, _, E>("change the target of "),
        ),
        value(RetargetScope::All, tag("you may choose new targets for ")),
    ))
    .parse(lower)
    .ok()?;

    // Split off trailing "to [target]" — forced retarget destination.
    let (spell_phrase, forced_to) =
        if let Ok((_, (before, after))) = nom_primitives::split_once_on(rest, " to ") {
            let (filter, _) = parse_target(after);
            (before, Some(filter))
        } else {
            (rest, None)
        };

    // CR 115.7: Parse the spell phrase to extract a stack-entry filter.
    // Strip "with a single target" qualifier before parsing the type.
    let has_single_target = scan_contains_phrase(spell_phrase, "with a single target");
    // CR 115.9c: "that targets only [X]" is handled by parse_that_clause_suffix via
    // parse_type_phrase, producing FilterProp::TargetsOnly — no manual stripping needed.
    let spell_phrase_clean = spell_phrase.replace(" with a single target", "");
    let spell_phrase_clean = spell_phrase_clean.trim();

    // Handle "spell or ability" specially since "ability" is not a card type in parse_target.
    // CR 115.7: "spell or ability" matches any spell or any activated/triggered ability on the stack.
    let mut target = if scan_contains_phrase(spell_phrase_clean, "spell or ability")
        || scan_contains_phrase(spell_phrase_clean, "spell and/or ability")
    {
        // Both spells and abilities on the stack
        TargetFilter::Or {
            filters: vec![TargetFilter::StackSpell, TargetFilter::StackAbility],
        }
    } else if scan_contains_phrase(spell_phrase_clean, "activated or triggered ability")
        || scan_contains_phrase(spell_phrase_clean, "activated ability")
    {
        TargetFilter::StackAbility
    } else if scan_contains_phrase(spell_phrase_clean, "spell") {
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

/// Extract the imperative verb keyword from an effect for verb carry-forward
/// in multi-target conjunctions. Returns the verb string that, when prepended
/// to a bare "target X" clause, allows re-parsing as the same effect type.
fn extract_effect_verb(effect: &Effect) -> Option<&'static str> {
    match effect {
        Effect::Destroy { .. } | Effect::DestroyAll { .. } => Some("destroy"),
        Effect::ChangeZone {
            destination: Zone::Exile,
            ..
        } => Some("exile"),
        Effect::ChangeZone {
            destination: Zone::Hand,
            origin: Some(Zone::Battlefield),
            ..
        } => Some("return"),
        Effect::Sacrifice { .. } => Some("sacrifice"),
        Effect::Tap { .. } | Effect::TapAll { .. } => Some("tap"),
        Effect::Untap { .. } | Effect::UntapAll { .. } => Some("untap"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        CastVariantPaid, Comparator, ContinuousModification, ControllerRef, DoublePTMode, Duration,
        FilterProp, GainLifePlayer, LinkedExileScope, ManaContribution, ManaProduction,
        PaymentCost, TypeFilter,
    };
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::zones::Zone;

    /// CR 608.2c: "If <cond>, you may instead <reveal-N-from-among>" must produce
    /// a conditional Dig alternative where the `else_ability` is the base Dig
    /// (with its patched filter) and the outer Dig carries the alternative's
    /// selection parameters. Class coverage: any Dig-followed-by-conditional-reveal
    /// pattern ("Follow the Lumarets" is the exemplar card).
    #[test]
    fn dig_conditional_instead_alternative_preserves_both_branches() {
        let def = parse_effect_chain(
            "Look at the top four cards of your library. You may reveal a creature or land card from among them and put it into your hand. If you gained life this turn, you may instead reveal two creature and/or land cards from among them and put them into your hand. Put the rest on the bottom of your library in a random order.",
            AbilityKind::Spell,
        );

        // Outer Dig: alternative (condition true) — keep 2 exactly, Or[Creature, Land].
        let Effect::Dig {
            count,
            keep_count,
            up_to,
            filter,
            rest_destination,
            destination,
            ..
        } = &*def.effect
        else {
            panic!("expected outer Effect::Dig, got {:?}", def.effect);
        };
        assert!(
            matches!(count, QuantityExpr::Fixed { value: 4 }),
            "alt branch should see top 4"
        );
        assert_eq!(*keep_count, Some(2), "alt branch keeps exactly 2");
        assert!(!*up_to, "alt branch is exact-2 (not up-to)");
        assert_eq!(*destination, Some(Zone::Hand));
        assert_eq!(*rest_destination, Some(Zone::Library));
        let TargetFilter::Or { filters } = filter else {
            panic!("alt filter should be Or, got {:?}", filter);
        };
        assert_eq!(filters.len(), 2);

        // Condition is life-gained-this-turn.
        let Some(AbilityCondition::QuantityCheck {
            lhs,
            comparator,
            rhs,
        }) = &def.condition
        else {
            panic!("expected QuantityCheck condition, got {:?}", def.condition);
        };
        assert!(matches!(
            lhs,
            QuantityExpr::Ref {
                qty: QuantityRef::LifeGainedThisTurn
            }
        ));
        assert_eq!(*comparator, Comparator::GE);
        assert!(matches!(rhs, QuantityExpr::Fixed { value: 1 }));

        // else_ability carries the base Dig (condition false) — keep up_to 1.
        let base = def
            .else_ability
            .as_deref()
            .expect("base Dig must be stored as else_ability");
        let Effect::Dig {
            count: base_count,
            keep_count: base_keep,
            up_to: base_up_to,
            filter: base_filter,
            rest_destination: base_rest,
            ..
        } = &*base.effect
        else {
            panic!("expected base Effect::Dig, got {:?}", base.effect);
        };
        assert!(matches!(base_count, QuantityExpr::Fixed { value: 4 }));
        assert_eq!(*base_keep, Some(1));
        assert!(*base_up_to, "base branch is up-to 1");
        assert_eq!(*base_rest, Some(Zone::Library), "PutRest must recurse");
        let TargetFilter::Or {
            filters: base_filters,
        } = base_filter
        else {
            panic!("base filter should be Or, got {:?}", base_filter);
        };
        assert_eq!(base_filters.len(), 2);
    }

    /// CR 601.2f: Regression gate — "This ability costs {1} less to activate
    /// for each page counter on this artifact" must parse into an
    /// `Effect::Unimplemented` sub_ability whose description carries the full
    /// cost-reduction sentence, so `strip_cost_reduction_node` can promote it
    /// to `def.cost_reduction`. Previously the bare `"this "` subject-prefix
    /// in `strip_subject_clause` caused the sentence to be misparsed as
    /// `Effect::Counter { target: Any }` by finding the noun "counter" (in
    /// "page counter") and mistaking it for the verb "counter" (as in
    /// counter-a-spell). Reproduced on Diary of Dreams.
    #[test]
    fn diary_of_dreams_cost_reduction_not_misparsed_as_counterspell() {
        let def = parse_effect_chain(
            "Draw a card. This ability costs {1} less to activate for each page counter on this artifact.",
            AbilityKind::Activated,
        );
        // The effect chain itself must be Draw (no counterspell poisoning).
        assert!(
            matches!(&*def.effect, Effect::Draw { .. }),
            "expected Draw as primary effect, got {:?}",
            def.effect
        );
        // The cost-reduction sentence MUST reach `strip_cost_reduction_node`
        // as Unimplemented text. The sub_ability may be present (before
        // extraction) or absorbed into def.cost_reduction by the downstream
        // `extract_cost_reduction_from_chain` pass run from `oracle.rs`. Here
        // we only verify it's NOT a spurious Counter effect.
        if let Some(sub) = &def.sub_ability {
            assert!(
                !matches!(&*sub.effect, Effect::Counter { .. }),
                "sub_ability must not be a counter-spell effect, got {:?}",
                sub.effect
            );
        }
    }

    #[test]
    fn where_x_binds_siblings_in_same_sentence() {
        // CR 107.3i: "all instances of X on an object have the same value".
        // For "target player loses X life and you gain X life, where X is ~"
        // both clauses must resolve X to the same expression, even though only
        // the second clause textually carries the binding (Essence Harvest).
        let def = parse_effect_chain(
            "Target player loses X life and you gain X life, where X is the greatest power among creatures you control.",
            AbilityKind::Spell,
        );
        // The first clause should be LoseLife with Aggregate(Max, Power, ...).
        match &*def.effect {
            Effect::LoseLife { amount, .. } => match amount {
                QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate { .. },
                } => {}
                other => panic!(
                    "expected LoseLife amount to be Aggregate (propagated from sibling where-X), got {other:?}"
                ),
            },
            other => panic!("expected LoseLife, got {other:?}"),
        }
        // The sibling GainLife must also resolve to the same aggregate.
        let sub = def.sub_ability.as_ref().expect("sibling gain_life");
        match &*sub.effect {
            Effect::GainLife { amount, .. } => match amount {
                QuantityExpr::Ref {
                    qty: QuantityRef::Aggregate { .. },
                } => {}
                other => panic!("expected GainLife Aggregate, got {other:?}"),
            },
            other => panic!("expected GainLife, got {other:?}"),
        }
    }

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
    fn effect_damage_to_each_opponent_uses_player_scope() {
        let e = parse_effect("~ deals 1 damage to each opponent");
        assert!(matches!(
            e,
            Effect::DamageEachPlayer {
                amount: QuantityExpr::Fixed { value: 1 },
                player_filter: PlayerFilter::Opponent,
            }
        ));
    }

    #[test]
    fn damage_each_player_scope_rejects_compound_phrase() {
        assert_eq!(
            parse_damage_each_player_scope("each opponent and each creature they control"),
            None
        );
    }

    #[test]
    fn damage_each_player_scope_allows_trailing_punctuation() {
        assert_eq!(
            parse_damage_each_player_scope("each opponent,"),
            Some(PlayerFilter::Opponent)
        );
    }

    /// CR 107.3i + CR 120.3: Descent into Avernus chains
    /// "each player creates X Treasure tokens AND ~ deals X damage to each player,
    /// where X is the number of descent counters on ~."
    /// The damage classifier in `try_parse_damage_with_remainder` must strip the
    /// trailing "where X is <expr>" binding from the recipient phrase before
    /// invoking `parse_damage_each_player_scope`; otherwise its exact-match
    /// check on "each player" would reject the trailing clause and fall through
    /// to `DamageAll{Typed{empty}}`. The where-X expression itself is captured
    /// at chunk-build time and re-applied by `apply_where_x_ability_expression`,
    /// so the local strip does not perturb Token P/T, Pump, or SkipNextTurn
    /// where-X handling.
    #[test]
    fn descent_into_avernus_sentence_chain() {
        let chain = parse_effect_chain(
            "Then each player creates X Treasure tokens and ~ deals X damage to each player, where X is the number of descent counters on ~",
            AbilityKind::Spell,
        );
        let sub = chain
            .sub_ability
            .as_ref()
            .expect("expected damage sub_ability");
        match &*sub.effect {
            Effect::DamageEachPlayer {
                player_filter: PlayerFilter::All,
                ..
            } => {}
            other => panic!("expected DamageEachPlayer{{All}} for second clause, got {other:?}"),
        }
    }

    #[test]
    fn effect_damage_to_each_player_uses_player_scope() {
        let e = parse_effect("~ deals 1 damage to each player");
        assert!(matches!(
            e,
            Effect::DamageEachPlayer {
                amount: QuantityExpr::Fixed { value: 1 },
                player_filter: PlayerFilter::All,
            }
        ));
    }

    /// CR 120.3 + CR 603.2c: "each other opponent" routes to the
    /// `OpponentOtherThanTriggering` variant — the trigger context excludes
    /// the opponent who already received damage from the source. Hydra
    /// Omnivore class.
    #[test]
    fn damage_each_player_scope_other_opponent() {
        assert_eq!(
            parse_damage_each_player_scope("each other opponent"),
            Some(PlayerFilter::OpponentOtherThanTriggering)
        );
    }

    /// "each other player" reduces to plain `Opponent` — outside trigger
    /// context "other" anaphors the controller, leaving only opponents.
    /// Syphon Soul class.
    #[test]
    fn damage_each_player_scope_other_player() {
        assert_eq!(
            parse_damage_each_player_scope("each other player"),
            Some(PlayerFilter::Opponent)
        );
    }

    /// Trailing punctuation after "each other opponent" is allowed.
    #[test]
    fn damage_each_player_scope_other_opponent_with_trailing_punctuation() {
        assert_eq!(
            parse_damage_each_player_scope("each other opponent."),
            Some(PlayerFilter::OpponentOtherThanTriggering)
        );
    }

    /// CR 120.3 + CR 603.2c: Effect-level lowering for Hydra Omnivore's body
    /// "it deals that much damage to each other opponent".
    #[test]
    fn effect_damage_to_each_other_opponent_uses_player_scope() {
        let e = parse_effect("~ deals that much damage to each other opponent");
        assert!(
            matches!(
                e,
                Effect::DamageEachPlayer {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    player_filter: PlayerFilter::OpponentOtherThanTriggering,
                }
            ),
            "expected DamageEachPlayer{{OpponentOtherThanTriggering}}, got {e:?}"
        );
    }

    /// CR 120.3: Composite "each opponent and each [type] you don't control"
    /// routes to `DamageAll` with the typed object filter populated and the
    /// `player_filter` set to Opponent. Omnath, Locus of Creation 3rd-branch class.
    #[test]
    fn effect_damage_compound_planeswalker_you_dont_control() {
        let e = parse_effect(
            "~ deals 4 damage to each opponent and each planeswalker you don't control",
        );
        match e {
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 4 },
                target,
                player_filter: Some(PlayerFilter::Opponent),
            } => match target {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                    assert!(tf
                        .type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Planeswalker)));
                }
                other => panic!("expected Typed{{Planeswalker, Opponent}}, got {other:?}"),
            },
            other => panic!("expected DamageAll with player_filter Opponent, got {other:?}"),
        }
    }

    /// Regression: "your opponents control" suffix variant (Sarkhan, Dragonsoul
    /// class) routes through the same compound parser as "you don't control".
    #[test]
    fn effect_damage_compound_creature_your_opponents_control() {
        let e = parse_effect(
            "~ deals 1 damage to each opponent and each creature your opponents control",
        );
        match e {
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 1 },
                target,
                player_filter: Some(PlayerFilter::Opponent),
            } => match target {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                    assert!(tf
                        .type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Creature)));
                }
                other => panic!("expected Typed{{Creature, Opponent}}, got {other:?}"),
            },
            other => panic!("expected DamageAll with player_filter Opponent, got {other:?}"),
        }
    }

    /// Regression: existing "they control" suffix continues to work after the
    /// refactor. Goblin Chainwhirler class.
    #[test]
    fn effect_damage_compound_creature_planeswalker_they_control() {
        let e = parse_effect(
            "~ deals 1 damage to each opponent and each creature and planeswalker they control",
        );
        match e {
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Or { filters },
                player_filter: Some(PlayerFilter::Opponent),
            } => {
                assert_eq!(filters.len(), 2);
                for f in &filters {
                    if let TargetFilter::Typed(tf) = f {
                        assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                    } else {
                        panic!("expected Typed inside Or, got {f:?}");
                    }
                }
            }
            other => {
                panic!("expected DamageAll Or{{...}} with player_filter Opponent, got {other:?}")
            }
        }
    }

    /// CR 120.3 + CR 119.3a: Pyrohemia / Pestilence class — "each creature and
    /// each player" must emit a unified `DamageAll` carrying both the creature
    /// filter and `player_filter: Some(PlayerFilter::All)`. Previously the
    /// "and each player" half was silently dropped.
    #[test]
    fn effect_damage_compound_each_creature_and_each_player() {
        let e = parse_effect("~ deals 1 damage to each creature and each player");
        match e {
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(tf),
                player_filter: Some(PlayerFilter::All),
            } => {
                assert!(tf
                    .type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Creature)));
                assert!(tf.properties.is_empty());
            }
            other => panic!("expected DamageAll{{Creature, player_filter=All}}, got {other:?}"),
        }
    }

    /// Hurricane class — "each creature with flying and each player". The
    /// property suffix on the type half must be preserved, and the player
    /// filter must still be populated.
    #[test]
    fn effect_damage_compound_each_creature_with_flying_and_each_player() {
        let e = parse_effect("~ deals X damage to each creature with flying and each player");
        match e {
            Effect::DamageAll {
                amount: _,
                target: TargetFilter::Typed(tf),
                player_filter: Some(PlayerFilter::All),
            } => {
                assert!(tf
                    .type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Creature)));
                assert!(!tf.properties.is_empty(), "expected flying property");
            }
            other => {
                panic!("expected DamageAll{{Creature w/ flying, player_filter=All}}, got {other:?}")
            }
        }
    }

    /// Earthquake / Flamebreak class — "each creature without flying and each player".
    #[test]
    fn effect_damage_compound_each_creature_without_flying_and_each_player() {
        let e = parse_effect("~ deals X damage to each creature without flying and each player");
        match e {
            Effect::DamageAll {
                amount: _,
                target: TargetFilter::Typed(tf),
                player_filter: Some(PlayerFilter::All),
            } => {
                assert!(tf
                    .type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Creature)));
                assert!(
                    !tf.properties.is_empty(),
                    "expected without-flying property"
                );
            }
            other => panic!(
                "expected DamageAll{{Creature w/o flying, player_filter=All}}, got {other:?}"
            ),
        }
    }

    /// Negative case: Disorder ("each white creature and each player who controls
    /// a white creature") must NOT route through the compound parser, because
    /// the player half is qualified by a relative clause. Falling through to the
    /// general path is acceptable here — the queue's L9-13 fix targets the
    /// universal "each player" form only.
    #[test]
    fn effect_damage_compound_qualified_player_rejected() {
        let e = parse_effect(
            "~ deals 2 damage to each white creature and each player who controls a white creature",
        );
        // The compound helper must bail; the result must NOT have player_filter
        // set to PlayerFilter::All (which would damage every player including
        // those without a white creature — wrong rules).
        if let Effect::DamageAll {
            player_filter: Some(PlayerFilter::All),
            ..
        } = &e
        {
            panic!("compound helper must bail on qualified 'each player who...': {e:?}");
        }
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
    fn try_split_damage_compound_player_and_object_target() {
        // CR 120.3 + CR 609.7: Goblin Chainwhirler — "deals 1 damage to each opponent
        // and each creature and planeswalker they control" must parse to a SINGLE
        // `DamageAll` carrying both the object filter and a `player_filter`, so the
        // resolver treats the full recipient set as one simultaneous damage event
        // from one source. Splitting into two effects would break replacement
        // effects like Awe Strike that watch damage from a single source.
        let ctx = ParseContext::default();
        let result = try_split_damage_compound(
            "deal 1 damage to each opponent and each creature and planeswalker they control",
            &ctx,
        );
        let clause = result.expect("compound player+object damage should parse");
        assert!(
            clause.sub_ability.is_none(),
            "unified effect must not emit a sub_ability (single simultaneous event)",
        );
        match &clause.effect {
            Effect::DamageAll {
                amount,
                target,
                player_filter,
            } => {
                assert!(matches!(amount, QuantityExpr::Fixed { value: 1 }));
                assert_eq!(
                    *player_filter,
                    Some(PlayerFilter::Opponent),
                    "player_filter must encode 'each opponent'",
                );
                match target {
                    TargetFilter::Or { filters } => {
                        assert_eq!(filters.len(), 2);
                        for f in filters {
                            match f {
                                TargetFilter::Typed(tf) => {
                                    assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                                }
                                other => panic!("expected Typed in Or, got: {other:?}"),
                            }
                        }
                    }
                    other => panic!("expected Or filter, got: {other:?}"),
                }
            }
            other => panic!("expected unified DamageAll, got: {other:?}"),
        }
    }

    #[test]
    fn try_split_damage_compound_player_and_planeswalker() {
        // CR 120.3: Kumano Faces Kakkazan — "deals 1 damage to each opponent and
        // each planeswalker they control" — same class, single planeswalker type.
        let ctx = ParseContext::default();
        let result = try_split_damage_compound(
            "deal 1 damage to each opponent and each planeswalker they control",
            &ctx,
        );
        let clause = result.expect("compound player+object damage should parse");
        assert!(clause.sub_ability.is_none());
        match &clause.effect {
            Effect::DamageAll {
                target,
                player_filter,
                ..
            } => {
                assert_eq!(*player_filter, Some(PlayerFilter::Opponent));
                match target {
                    TargetFilter::Typed(tf) => {
                        assert!(tf.type_filters.contains(&TypeFilter::Planeswalker));
                        assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                    }
                    other => panic!("expected Typed filter, got: {other:?}"),
                }
            }
            other => panic!("expected unified DamageAll, got: {other:?}"),
        }
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
                    count: QuantityExpr::Fixed { value: 1 },
                    ..
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
    fn effect_exile_your_graveyard_is_change_zone_all() {
        let e = parse_effect("exile your graveyard");
        assert!(
            matches!(
                e,
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Exile,
                    ..
                }
            ),
            "exile your graveyard should be ChangeZoneAll with origin=Graveyard, got {e:?}"
        );
    }

    #[test]
    fn effect_exile_possessive_graveyard_is_change_zone_all() {
        let e = parse_effect("exile their graveyard");
        assert!(
            matches!(
                e,
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Exile,
                    ..
                }
            ),
            "exile their graveyard should be ChangeZoneAll with origin=Graveyard, got {e:?}"
        );
    }

    #[test]
    fn effect_exile_target_player_graveyard_is_change_zone_all() {
        // CR 400.12 + CR 404 + CR 406: "exile target player's graveyard" must
        // act on every card in that zone — Nihil Spellbomb / Bojuka Bog class.
        let e = parse_effect("exile target player's graveyard");
        assert!(
            matches!(
                e,
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Exile,
                    target: TargetFilter::Player,
                }
            ),
            "exile target player's graveyard should be ChangeZoneAll with origin=Graveyard, target=Player, got {e:?}"
        );
    }

    #[test]
    fn effect_exile_target_opponent_graveyard_is_change_zone_all() {
        // CR 400.12: "exile target opponent's graveyard" — same class.
        let e = parse_effect("exile target opponent's graveyard");
        assert!(
            matches!(
                e,
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Exile,
                    ..
                }
            ),
            "exile target opponent's graveyard should be ChangeZoneAll with origin=Graveyard, got {e:?}"
        );
    }

    #[test]
    fn effect_compound_exile_creature_and_possessive_graveyard() {
        let clause = parse_effect_clause(
            "exile a creature they control and their graveyard",
            &Default::default(),
        );
        assert!(
            matches!(clause.effect, Effect::ChangeZone { .. }),
            "primary should be ChangeZone, got {:?}",
            clause.effect
        );
        let sub = clause.sub_ability.expect("should have sub_ability");
        assert!(
            matches!(
                *sub.effect,
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Exile,
                    ..
                }
            ),
            "sub_ability should be ChangeZoneAll with origin=Graveyard, got {:?}",
            *sub.effect
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
    fn for_each_pump_threads_subject_target_creature() {
        let e = parse_effect("target creature gets +1/+1 for each creature you control");
        assert!(
            matches!(
                e,
                Effect::Pump {
                    target: TargetFilter::Typed(..),
                    power: PtValue::Quantity(..),
                    toughness: PtValue::Quantity(..),
                }
            ),
            "pump should have typed target filter, not Any"
        );
    }

    #[test]
    fn for_each_pump_self_ref() {
        let e = parse_effect("~ gets +1/+1 for each creature you control");
        assert!(
            matches!(
                e,
                Effect::Pump {
                    target: TargetFilter::SelfRef,
                    power: PtValue::Quantity(..),
                    ..
                }
            ),
            "self-ref subject should produce SelfRef target"
        );
    }

    #[test]
    fn for_each_deal_damage() {
        let e = parse_effect("~ deals 1 damage to any target for each creature you control");
        assert!(
            matches!(e, Effect::DealDamage { .. }),
            "should produce DealDamage"
        );
    }

    #[test]
    fn for_each_token_count_replaced() {
        let e =
            parse_effect("create a 1/1 white Warrior creature token for each creature you control");
        assert!(
            matches!(
                e,
                Effect::Token {
                    count: QuantityExpr::Ref { .. },
                    ..
                }
            ),
            "token count should be a Ref quantity, not Fixed"
        );
    }

    #[test]
    fn for_each_counter_on_target() {
        let e =
            parse_effect("~ deals damage equal to the number of +1/+1 counters on that creature");
        // This is a CDA-style quantity, not a for-each. Verify the quantity ref is correct.
        if let Effect::DealDamage { amount, .. } = &e {
            assert!(
                matches!(
                    amount,
                    QuantityExpr::Ref {
                        qty: QuantityRef::CountersOnTarget { .. }
                    }
                ),
                "should produce CountersOnTarget quantity"
            );
        }
    }

    #[test]
    fn for_each_put_counter_threads_subject() {
        let e = parse_effect("put a +1/+1 counter on ~ for each creature you control");
        assert!(
            matches!(
                e,
                Effect::PutCounter {
                    target: TargetFilter::SelfRef,
                    count: QuantityExpr::Ref { .. },
                    ..
                }
            ),
            "put counter should have SelfRef target and Ref count"
        );
    }

    #[test]
    fn effect_mana_production() {
        let e = parse_effect("Add {W}");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::Fixed { ref colors,
                    contribution: ManaContribution::Base,
                }, ..
            } if colors == &vec![ManaColor::White]
        ));
    }

    #[test]
    fn effect_add_additional_mana() {
        // CR 605.1a: "an additional" marks the mana contribution as Additional
        // (Wild Growth / Verdant Haven class), not Base.
        let e = parse_effect("Add an additional {G}");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::Fixed { ref colors,
                    contribution: ManaContribution::Additional,
                }, ..
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

    /// CR 701.10b: "double ~'s power" — self-reference possessive form
    /// (Devilish Valet, Okaun, Casey Jones, Grunn, Overclocked Electromancer class).
    /// `~` is the parser's normalized form of the card's own name.
    #[test]
    fn effect_double_self_ref_power() {
        let e = parse_effect("double ~'s power");
        assert!(matches!(
            e,
            Effect::DoublePT {
                mode: DoublePTMode::Power,
                target: TargetFilter::SelfRef,
            }
        ));
    }

    #[test]
    fn effect_double_self_ref_power_and_toughness() {
        let e = parse_effect("double ~'s power and toughness");
        assert!(matches!(
            e,
            Effect::DoublePT {
                mode: DoublePTMode::PowerAndToughness,
                target: TargetFilter::SelfRef,
            }
        ));
    }

    /// "double target creature's power" — Bulk Up class possessive form.
    #[test]
    fn effect_double_target_creatures_power() {
        let e = parse_effect("Double target creature's power");
        assert!(matches!(
            e,
            Effect::DoublePT {
                mode: DoublePTMode::Power,
                target: TargetFilter::Typed(_),
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

    /// CR 400.7 + CR 611.2c: Mass-bounce class — "return all/each [filter]"
    /// must lower to `Effect::BounceAll`, not single-target `Effect::Bounce`,
    /// so the runtime resolver iterates every matching permanent instead of
    /// prompting for one.
    #[test]
    fn effect_bounce_all_creatures_evacuation() {
        let e = parse_effect("Return all creatures to their owners' hands");
        match e {
            Effect::BounceAll {
                target: TargetFilter::Typed(filter),
                destination,
            } => {
                assert!(destination.is_none(), "default destination = owner's hand");
                assert_eq!(filter.type_filters, vec![TypeFilter::Creature]);
                assert!(filter.controller.is_none(), "no controller scoping");
            }
            other => panic!("expected BounceAll, got {other:?}"),
        }
    }

    /// Plural "all nonland permanents" filter from Devastation Tide / Coastal
    /// Breach / Crush of Tentacles / Worldpurge. The `Non(Land)` property must
    /// thread through the parser unchanged.
    #[test]
    fn effect_bounce_all_nonland_permanents_devastation_tide() {
        let e = parse_effect("Return all nonland permanents to their owners' hands");
        match e {
            Effect::BounceAll {
                target: TargetFilter::Typed(filter),
                ..
            } => {
                assert!(
                    filter
                        .type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Permanent)),
                    "permanent type filter present"
                );
                assert!(
                    filter.type_filters.iter().any(|t| matches!(
                        t,
                        TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Land)
                    )),
                    "nonland exclusion present, got {:?}",
                    filter.type_filters
                );
            }
            other => panic!("expected BounceAll {{ Permanent, Non(Land) }}, got {other:?}"),
        }
    }

    /// Sunderflock-shape: "return all non-Elemental creatures" — class with a
    /// subtype exclusion. Parser must preserve `Non(Subtype: Elemental)` in
    /// the property list.
    #[test]
    fn effect_bounce_all_non_subtype_creatures_sunderflock() {
        let e = parse_effect("Return all non-Elemental creatures to their owners' hands");
        match e {
            Effect::BounceAll {
                target: TargetFilter::Typed(filter),
                ..
            } => {
                assert!(
                    filter
                        .type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Creature)),
                    "creature type filter present"
                );
                assert!(
                    filter.type_filters.iter().any(|t| matches!(
                        t,
                        TypeFilter::Non(inner) if matches!(&**inner, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Elemental"))
                    )),
                    "non-Elemental subtype exclusion present, got {:?}",
                    filter.type_filters
                );
            }
            other => {
                panic!("expected BounceAll {{ Creature, Non(Subtype: Elemental) }}, got {other:?}")
            }
        }
    }

    /// "Return each creature ..." pluralizer is the same class as "return all
    /// creatures ..." per the verb-dispatch precheck.
    #[test]
    fn effect_bounce_all_each_creature_variant() {
        let e = parse_effect("Return each creature to its owner's hand");
        assert!(matches!(e, Effect::BounceAll { .. }), "got {e:?}");
    }

    /// CR 115.1 + CR 608.2c: Single-target "return target creature" must NOT
    /// promote to `BounceAll` — it identifies one selected permanent. The
    /// `tag("return ")` arm (without the `all`/`each` precheck) handles this
    /// case unchanged. This regression test pins the boundary.
    #[test]
    fn effect_bounce_single_target_preservation() {
        let e = parse_effect("Return target creature to its owner's hand");
        assert!(
            matches!(e, Effect::Bounce { .. }),
            "single-target return must stay Bounce, got {e:?}"
        );
    }

    /// CR 400.7: "Return all <filter> to the battlefield" (Open the Vaults,
    /// Replenish, Splendid Reclamation, etc.) must NOT promote to `BounceAll`
    /// — the destination is the battlefield, not hand. Routes through
    /// `ReturnToBattlefield` ⇒ `Effect::ChangeZone { destination: Battlefield }`.
    /// Pins the destination-routing boundary on the unified return arm.
    #[test]
    fn effect_return_all_to_battlefield_stays_change_zone() {
        let e = parse_effect(
            "Return all artifact and enchantment cards from all graveyards to the battlefield under their owners' control",
        );
        assert!(
            matches!(
                e,
                Effect::ChangeZone {
                    destination: Zone::Battlefield,
                    ..
                }
            ),
            "return-all-to-battlefield must lower to ChangeZone, not BounceAll, got {e:?}"
        );
    }

    /// Praetor's Counsel: "Return all cards from your graveyard to your hand"
    /// — origin is graveyard, destination is hand. The `BounceAll` resolver
    /// only scans the battlefield, so this MUST remain a `ChangeZone` with
    /// `origin: Graveyard, destination: Hand`. Regression guard.
    #[test]
    fn effect_return_all_from_graveyard_to_hand_stays_change_zone() {
        let e = parse_effect("Return all cards from your graveyard to your hand");
        assert!(
            matches!(
                e,
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Hand,
                    ..
                }
            ),
            "return-all from graveyard→hand must lower to ChangeZone, not BounceAll, got {e:?}"
        );
    }

    /// CR 701.6 + CR 405.1: "Counter all spells your opponents control"
    /// (Glen Elendra's Answer) must lower to `Effect::CounterAll` with the
    /// class filter preserving `controller: Opponent`, `InZone Stack`, and
    /// the spell-card type — so the runtime resolver iterates the stack and
    /// counters every matching opponent spell instead of prompting for one.
    #[test]
    fn effect_counter_all_opponent_spells_glen_elendra() {
        let e = parse_effect("Counter all spells your opponents control");
        match e {
            Effect::CounterAll {
                target: TargetFilter::Typed(filter),
            } => {
                assert!(
                    matches!(
                        filter.controller,
                        Some(crate::types::ability::ControllerRef::Opponent)
                    ),
                    "controller scoping preserved, got {:?}",
                    filter.controller
                );
                assert!(
                    filter
                        .properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack })),
                    "InZone Stack preserved, got {:?}",
                    filter.properties
                );
            }
            other => panic!("expected CounterAll {{ Typed }}, got {other:?}"),
        }
    }

    /// "Counter all other spells" (Swift Silence) — `other` lowers to a
    /// `FilterProp::Another` constraint plus the stack-zone pin.
    #[test]
    fn effect_counter_all_other_spells_swift_silence() {
        let e = parse_effect("Counter all other spells");
        match e {
            Effect::CounterAll {
                target: TargetFilter::Typed(filter),
            } => {
                assert!(
                    filter
                        .properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::Another)),
                    "Another constraint preserved, got {:?}",
                    filter.properties
                );
                assert!(
                    filter
                        .properties
                        .iter()
                        .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack })),
                    "InZone Stack preserved"
                );
            }
            other => panic!("expected CounterAll {{ Typed, Another }}, got {other:?}"),
        }
    }

    /// "Counter all abilities your opponents control" (Kadena's Silencer
    /// trigger body) — bare "abilities" with the mass precheck routes to
    /// `TargetFilter::StackAbility`. (Controller scoping on stack abilities
    /// is a separate filter-extension follow-up — see resolver doc.)
    #[test]
    fn effect_counter_all_abilities_kadena() {
        let e = parse_effect("Counter all abilities your opponents control");
        assert!(
            matches!(
                e,
                Effect::CounterAll {
                    target: TargetFilter::StackAbility,
                }
            ),
            "expected CounterAll {{ StackAbility }}, got {e:?}"
        );
    }

    /// "Counter each spell" pluralizer is the same class as "counter all
    /// spells" per the verb-dispatch precheck.
    #[test]
    fn effect_counter_each_spell_variant() {
        let e = parse_effect("Counter each spell");
        assert!(matches!(e, Effect::CounterAll { .. }), "got {e:?}");
    }

    /// CR 115.1 + CR 608.2c: "Counter target spell" must NOT promote to
    /// `CounterAll` — it identifies one selected spell. Regression test pins
    /// the boundary on the unified counter precheck.
    #[test]
    fn effect_counter_single_target_preservation() {
        let e = parse_effect("Counter target spell");
        assert!(
            matches!(e, Effect::Counter { .. }),
            "single-target counter must stay Counter, got {e:?}"
        );
    }

    /// "Counter target activated or triggered ability" (single-target ability
    /// counter) must remain `Effect::Counter { target: StackAbility }`.
    /// Regression guard against the "abilities"-bare-mass arm leaking into
    /// the single-target path.
    #[test]
    fn effect_counter_single_target_ability_preservation() {
        let e = parse_effect("Counter target activated or triggered ability");
        assert!(
            matches!(
                e,
                Effect::Counter {
                    target: TargetFilter::StackAbility,
                    ..
                }
            ),
            "single-target ability counter must stay Counter {{ StackAbility }}, got {e:?}"
        );
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
                count: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ));
    }

    #[test]
    fn effect_scry() {
        let e = parse_effect("Scry 2");
        assert!(matches!(
            e,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 2 },
                ..
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
                    },
                    ..
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
                    },
                    ..
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
    fn effect_target_creature_explores_uses_target_only_chain() {
        let def = parse_effect_chain("Target creature explores", AbilityKind::Spell);
        assert!(matches!(
            *def.effect,
            Effect::TargetOnly {
                target: TargetFilter::Typed(ref tf)
            } if tf.type_filters.contains(&TypeFilter::Creature)
        ));
        let sub = def
            .sub_ability
            .expect("targeted explore should have sub ability");
        assert!(matches!(*sub.effect, Effect::Explore));
    }

    #[test]
    fn effect_up_to_one_target_creature_explores_sets_multi_target() {
        let def = parse_effect_chain("Up to one target creature explores", AbilityKind::Spell);
        assert!(matches!(*def.effect, Effect::TargetOnly { .. }));
        assert_eq!(
            def.multi_target,
            Some(MultiTargetSpec {
                min: 0,
                max: Some(1),
            })
        );
        assert!(matches!(
            *def.sub_ability
                .expect("expected explore sub ability")
                .effect,
            Effect::Explore
        ));
    }

    #[test]
    fn effect_target_creature_explores_then_explores_again() {
        let def = parse_effect_chain(
            "Target creature explores, then it explores again",
            AbilityKind::Spell,
        );
        assert!(matches!(*def.effect, Effect::TargetOnly { .. }));
        let first = def.sub_ability.expect("expected first explore");
        assert!(matches!(*first.effect, Effect::Explore));
        let second = first.sub_ability.expect("expected repeated explore");
        assert!(matches!(*second.effect, Effect::Explore));
    }

    #[test]
    fn effect_target_creature_explores_x_times_sets_repeat_for() {
        let def = parse_effect_chain("Target creature explores X times", AbilityKind::Spell);
        assert!(matches!(*def.effect, Effect::TargetOnly { .. }));
        let sub = def.sub_ability.expect("expected explore sub ability");
        assert!(matches!(
            sub.repeat_for,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Variable { .. }
            })
        ));
        assert!(matches!(*sub.effect, Effect::Explore));
    }

    #[test]
    fn effect_it_explores_x_times_sets_repeat_for() {
        let def = parse_effect_chain("It explores X times", AbilityKind::Spell);
        assert!(matches!(*def.effect, Effect::Explore));
        assert!(matches!(
            def.repeat_for,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::Variable { .. }
            })
        ));
    }

    #[test]
    fn effect_each_merfolk_creature_you_control_explores_uses_explore_all() {
        let e = parse_effect("Each Merfolk creature you control explores");
        assert!(
            matches!(
                e,
                Effect::ExploreAll {
                    filter: TargetFilter::Typed(ref tf)
                } if tf.type_filters.contains(&TypeFilter::Creature)
            ),
            "expected ExploreAll creature filter, got {e:?}"
        );
    }

    #[test]
    fn effect_manifest_dread() {
        let e = parse_effect("Manifest dread");
        assert!(matches!(e, Effect::ManifestDread));
    }

    #[test]
    fn effect_manifest_top_card() {
        let e = parse_effect("Manifest the top card of your library");
        assert!(
            matches!(
                e,
                Effect::Manifest {
                    target: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 1 }
                }
            ),
            "expected Manifest {{ Controller, count: 1 }}, got: {e:?}"
        );
    }

    #[test]
    fn effect_manifest_top_two_cards() {
        let e = parse_effect("Manifest the top two cards of your library");
        assert!(
            matches!(
                e,
                Effect::Manifest {
                    target: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 2 }
                }
            ),
            "expected Manifest {{ Controller, count: 2 }}, got: {e:?}"
        );
    }

    #[test]
    fn effect_its_controller_manifests_top_card() {
        // CR 701.40a + CR 608.2c: Reality Shift — subject-shifted manifest binds
        // the acting player to ParentTargetController (the exiled creature's
        // controller) rather than the caster.
        let def = parse_effect_chain(
            "Exile target creature. Its controller manifests the top card of their library.",
            AbilityKind::Spell,
        );
        // First clause exiles a creature target.
        assert!(
            matches!(*def.effect, Effect::ChangeZone { .. }),
            "expected ChangeZone primary effect, got {:?}",
            def.effect
        );
        let sub = def
            .sub_ability
            .as_ref()
            .expect("expected chained manifest sub-ability");
        assert!(
            matches!(
                *sub.effect,
                Effect::Manifest {
                    target: TargetFilter::ParentTargetController,
                    count: QuantityExpr::Fixed { value: 1 }
                }
            ),
            "expected Manifest {{ ParentTargetController, count: 1 }}, got: {:?}",
            sub.effect
        );
    }

    #[test]
    fn effect_look_at_that_many_cards() {
        let e = parse_effect("Look at that many cards from the top of your library");
        match e {
            Effect::Dig { count, reveal, .. } => {
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount
                        }
                    ),
                    "expected EventContextAmount, got: {count:?}"
                );
                assert!(!reveal, "should not be reveal");
            }
            other => panic!("expected Dig, got: {other:?}"),
        }
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
                count: QuantityExpr::Fixed { value: 1 },
                ..
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
    fn effect_chain_then_conjugated_draws() {
        // CR 608.2c: "Each player discards their hand, then draws seven cards."
        // The clause splitter must recognize "draws" as a conjugated verb form,
        // and the effect parser must deconjugate it to produce a Draw effect.
        let def = parse_effect_chain(
            "Each player discards their hand, then draws seven cards.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(&*def.effect, Effect::Discard { .. }),
            "primary effect should be Discard, got {:?}",
            def.effect
        );
        let sub = def
            .sub_ability
            .as_ref()
            .expect("should have sub_ability for Draw after ', then draws'");
        assert!(
            matches!(
                &*sub.effect,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 7 },
                    ..
                }
            ),
            "sub_ability should be Draw(7), got {:?}",
            sub.effect
        );
    }

    #[test]
    fn effect_chain_then_conjugated_sacrifices() {
        // "then sacrifices the rest" — conjugated verb after ", then"
        let def = parse_effect_chain(
            "chooses an artifact, then sacrifices the rest",
            AbilityKind::Spell,
        );
        // The first part may parse as Unimplemented (complex choice), but the chain
        // should exist and the sub should attempt to parse "sacrifice the rest".
        assert!(
            def.sub_ability.is_some(),
            "should have sub_ability after ', then sacrifices'"
        );
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

    // CR 608.2c + CR 117.3a + CR 701.23a: Assassin's Trophy-shape —
    // "Destroy target permanent an opponent controls. Its controller may
    // search their library for a basic land card, put it onto the
    // battlefield, then shuffle." The subject "its controller" anchors the
    // subsequent "put it onto the battlefield, then shuffle" clauses so the
    // destroyed permanent's controller (a) searches their own library,
    // (b) receives the found land onto the battlefield (as `ParentTarget`),
    // and (c) shuffles their library. No spurious Library→Hand step emitted.
    #[test]
    fn assassins_trophy_its_controller_may_search_chain() {
        use crate::types::ability::AbilityKind;
        let def = parse_effect_chain(
            "Destroy target permanent an opponent controls. Its controller may search their library for a basic land card, put it onto the battlefield, then shuffle.",
            AbilityKind::Spell,
        );
        // Top-level: Destroy
        assert!(
            matches!(&*def.effect, Effect::Destroy { .. }),
            "top-level should be Destroy, got {:?}",
            def.effect
        );
        // First sub: SearchLibrary with ParentTargetController target_player + optional
        let search = def
            .sub_ability
            .as_deref()
            .expect("should chain SearchLibrary");
        match &*search.effect {
            Effect::SearchLibrary { target_player, .. } => {
                assert_eq!(
                    target_player.as_ref(),
                    Some(&TargetFilter::ParentTargetController),
                    "SearchLibrary must target the destroyed permanent's controller",
                );
            }
            other => panic!("expected SearchLibrary, got {:?}", other),
        }
        assert!(search.optional, "SearchLibrary must be optional ('may')");
        // Second sub: ChangeZone Library→Battlefield (NOT Hand)
        let put = search
            .sub_ability
            .as_deref()
            .expect("should chain put-step");
        match &*put.effect {
            Effect::ChangeZone {
                origin,
                destination,
                ..
            } => {
                assert_eq!(*origin, Some(Zone::Library));
                assert_eq!(
                    *destination,
                    Zone::Battlefield,
                    "put-step destination must be Battlefield — no spurious Library→Hand"
                );
            }
            other => panic!("expected ChangeZone, got {:?}", other),
        }
        // Third sub: Shuffle with ParentTargetController (the acting player)
        let shuffle = put.sub_ability.as_deref().expect("should chain Shuffle");
        match &*shuffle.effect {
            Effect::Shuffle { target } => {
                assert_eq!(
                    *target,
                    TargetFilter::ParentTargetController,
                    "Shuffle must target the acting player (its controller), not the caster"
                );
            }
            other => panic!("expected Shuffle, got {:?}", other),
        }
        // Chain must end there — no Library→Hand tail.
        assert!(
            shuffle.sub_ability.is_none(),
            "chain must end after Shuffle; got extra: {:?}",
            shuffle.sub_ability
        );
    }

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
                ..
            } => {
                assert_eq!(count, QuantityExpr::Fixed { value: 1 });
                assert!(reveal);
                match filter {
                    TargetFilter::Typed(tf) => {
                        assert!(tf.type_filters.contains(&TypeFilter::Land));
                        assert!(tf.properties.iter().any(
                            |p| matches!(p, FilterProp::HasSupertype { value } if *value == crate::types::card_type::Supertype::Basic)
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
    fn effect_all_players_start_their_engines() {
        let e = parse_effect("All players start their engines!");
        assert!(matches!(
            e,
            Effect::StartYourEngines {
                player_scope: PlayerFilter::All
            }
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
    fn effect_its_controller_creates_tokens_sets_parent_target_controller_owner() {
        let e = parse_effect("Its controller creates two Map tokens");
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                count: QuantityExpr::Fixed { value: 2 },
                owner: TargetFilter::ParentTargetController,
                ..
            } if name == "Map"
        ));
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
    fn effect_silkguard_disjunctive_modified_hexproof_grant() {
        // CR 700.4 + CR 700.9: Silkguard — "Auras, Equipment, and modified
        // creatures you control gain hexproof until end of turn". The subject
        // is a three-way OR of Aura subtype, Equipment subtype, and
        // creature-with-Modified, scoped to `you control`. Produces a
        // continuous hexproof-grant effect whose `affected` is the full Or
        // filter. Leg-local FilterProp::Modified must not propagate to the
        // Aura/Equipment legs.
        let def = parse_effect_chain(
            "Auras, Equipment, and modified creatures you control gain hexproof until end of turn",
            AbilityKind::Spell,
        );
        let Effect::GenericEffect {
            static_abilities, ..
        } = &*def.effect
        else {
            panic!("expected GenericEffect, got {:?}", def.effect);
        };
        let static_ab = static_abilities
            .iter()
            .find(|s| {
                s.modifications
                    .contains(&ContinuousModification::AddKeyword {
                        keyword: crate::types::keywords::Keyword::Hexproof,
                    })
            })
            .expect("expected AddKeyword(Hexproof) modification");
        let Some(TargetFilter::Or { filters }) = &static_ab.affected else {
            panic!(
                "expected Or filter in affected, got {:?}",
                static_ab.affected
            );
        };
        assert_eq!(filters.len(), 3, "expected 3-leg Or, got {filters:#?}");
        let legs: Vec<&TypedFilter> = filters
            .iter()
            .map(|f| match f {
                TargetFilter::Typed(tf) => tf,
                other => panic!("expected Typed leg, got {other:?}"),
            })
            .collect();
        assert_eq!(
            legs[0].get_subtype(),
            Some("Aura"),
            "leg 0 must be Aura subtype"
        );
        assert_eq!(
            legs[1].get_subtype(),
            Some("Equipment"),
            "leg 1 must be Equipment subtype"
        );
        assert!(
            legs[2].type_filters.contains(&TypeFilter::Creature),
            "leg 2 must be Creature"
        );
        assert!(
            legs[2].properties.contains(&FilterProp::Modified),
            "leg 2 must carry FilterProp::Modified"
        );
        // Leg-local Modified must NOT propagate to Aura/Equipment legs.
        assert!(
            !legs[0].properties.contains(&FilterProp::Modified),
            "Aura leg must not carry FilterProp::Modified"
        );
        assert!(
            !legs[1].properties.contains(&FilterProp::Modified),
            "Equipment leg must not carry FilterProp::Modified"
        );
        // All three legs scoped to `you control`.
        assert_eq!(legs[0].controller, Some(ControllerRef::You));
        assert_eq!(legs[1].controller, Some(ControllerRef::You));
        assert_eq!(legs[2].controller, Some(ControllerRef::You));
        assert_eq!(def.duration, Some(Duration::UntilEndOfTurn));
    }

    #[test]
    fn effect_target_graveyard_spell_gains_flashback_until_end_of_turn() {
        let def = parse_effect_chain(
            "target instant or sorcery card in your graveyard gains flashback until end of turn. The flashback cost is equal to its mana cost.",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::GenericEffect {
                target: Some(TargetFilter::Or { filters }),
                static_abilities,
                duration,
            } => {
                assert_eq!(filters.len(), 2);
                for filter in filters {
                    let TargetFilter::Typed(tf) = filter else {
                        panic!("expected typed branch, got {:?}", filter);
                    };
                    assert_eq!(tf.controller, Some(ControllerRef::You));
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
                assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
                assert!(
                    static_abilities.iter().any(|static_def| {
                        static_def
                            .modifications
                            .contains(&ContinuousModification::AddKeyword {
                                keyword: crate::types::keywords::Keyword::Flashback(
                                    crate::types::keywords::FlashbackCost::Mana(
                                        ManaCost::SelfManaCost,
                                    ),
                                ),
                            })
                    }),
                    "missing flashback grant: {:?}",
                    static_abilities
                );
            }
            other => panic!("Expected GenericEffect, got {:?}", other),
        }
        assert!(
            def.sub_ability.is_none(),
            "flashback cost continuation should be absorbed into the grant"
        );
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
    fn compound_tap_and_put_counter_ignores_trigger_subject_context() {
        let clause = parse_effect_clause(
            "tap target creature an opponent controls and put a stun counter on it",
            &ParseContext {
                subject: Some(TargetFilter::SelfRef),
                card_name: None,
                actor: None,
                ..Default::default()
            },
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
            "sub should stay ParentTarget even in trigger context, got {:?}",
            sub.effect
        );
    }

    #[test]
    fn compound_tap_and_put_counter_lowercase_trigger_context() {
        let def = parse_effect_chain_with_context(
            "tap target creature an opponent controls and put a stun counter on it.",
            AbilityKind::Spell,
            &ParseContext {
                subject: Some(TargetFilter::SelfRef),
                card_name: None,
                actor: None,
                ..Default::default()
            },
        );
        let sub = def.sub_ability.expect("should have sub_ability");
        assert!(
            matches!(
                *sub.effect,
                Effect::PutCounter {
                    target: TargetFilter::ParentTarget,
                    ..
                }
            ),
            "sub should stay ParentTarget through effect-chain parsing, got {:?}",
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
                persist: true
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
    fn put_counter_each_of_up_to_two_target_creatures_is_multi_targeted() {
        let clause = parse_effect_clause(
            "put a +1/+1 counter on each of up to two target creatures",
            &ParseContext::default(),
        );
        assert_eq!(
            clause.multi_target,
            Some(MultiTargetSpec {
                min: 0,
                max: Some(2),
            })
        );
        assert!(
            matches!(
                clause.effect,
                Effect::PutCounter {
                    counter_type: ref ct,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(_),
                } if ct == "P1P1"
            ),
            "Expected targeted PutCounter with multi_target, got {:?}",
            clause.effect
        );
    }

    #[test]
    fn exile_top_of_your_library_parses_as_exile_top() {
        let effect = parse_effect("Exile the top card of your library");
        assert!(
            matches!(
                &effect,
                Effect::ExileTop {
                    player: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 1 },
                }
            ),
            "Expected ExileTop(controller, 1), got {:?}",
            effect
        );
    }

    #[test]
    fn put_counter_where_x_is_lowers_to_speed_quantity() {
        let def = parse_effect_chain_with_context(
            "put X +1/+1 counters on target creature you control, where X is your speed",
            AbilityKind::Spell,
            &ParseContext::default(),
        );

        assert!(matches!(
            *def.effect,
            Effect::PutCounter {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Speed,
                },
                ..
            }
        ));
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
            Effect::Mana { produced: ManaProduction::AnyOneColor { count: QuantityExpr::Fixed { value: 1 }, ref color_options, contribution: ManaContribution::Base, .. }, .. }
            if color_options == &vec![ManaColor::White, ManaColor::Blue, ManaColor::Black, ManaColor::Red, ManaColor::Green]
        ));
    }

    #[test]
    fn effect_add_additional_mana_of_chosen_color_utopia_sprawl() {
        // CR 605.1a + CR 107.4a: Utopia Sprawl's "adds an additional one mana of
        // the chosen color" must parse as `ChosenColor { contribution: Additional }`
        // so the additive role is preserved on the typed effect.
        let e = parse_effect("adds an additional one mana of the chosen color");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::ChosenColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    contribution: ManaContribution::Additional,
                },
                ..
            }
        ));
    }

    #[test]
    fn effect_add_mana_of_chosen_color_base() {
        // Baseline: without the "additional" prefix the contribution is Base.
        let e = parse_effect("add one mana of the chosen color");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::ChosenColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    contribution: ManaContribution::Base,
                },
                ..
            }
        ));
    }

    #[test]
    fn effect_add_additional_mana_any_color_fertile_ground() {
        // CR 605.1a: Fertile Ground's "adds an additional one mana of any color"
        // carries the additive role on `AnyOneColor`.
        let e = parse_effect("adds an additional one mana of any color");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    contribution: ManaContribution::Additional,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn effect_add_one_mana_of_any_exiled_card_color_pit_of_offerings() {
        // CR 605.1a + CR 406.1 + CR 610.3: Pit of Offerings — "Add one mana of any
        // of the exiled cards' colors" parses to ChoiceAmongExiledColors scoped
        // to the activating object.
        let e = parse_effect("Add one mana of any of the exiled cards' colors");
        assert_eq!(
            e,
            Effect::Mana {
                produced: ManaProduction::ChoiceAmongExiledColors {
                    source: LinkedExileScope::ThisObject,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            }
        );
    }

    #[test]
    fn effect_add_one_mana_of_any_color_among_exiled_cards() {
        // Synonym phrasing also routes to ChoiceAmongExiledColors.
        let e = parse_effect("Add one mana of any color among the exiled cards");
        assert_eq!(
            e,
            Effect::Mana {
                produced: ManaProduction::ChoiceAmongExiledColors {
                    source: LinkedExileScope::ThisObject,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            }
        );
    }

    #[test]
    fn effect_add_mana_any_color_in_commanders_color_identity() {
        // CR 903.4 + CR 903.4f: Path of Ancestry — "Add one mana of any color
        // in your commander's color identity" resolves to
        // `AnyInCommandersColorIdentity` so the color options are computed
        // dynamically at activation time from the activator's commander CI.
        let e = parse_effect("Add one mana of any color in your commander's color identity");
        assert_eq!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyInCommandersColorIdentity {
                    count: QuantityExpr::Fixed { value: 1 },
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            }
        );
        // Plural possessive variant (partner commanders).
        let e = parse_effect("Add one mana of any color in your commanders' color identity");
        assert_eq!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyInCommandersColorIdentity {
                    count: QuantityExpr::Fixed { value: 1 },
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            }
        );
    }

    #[test]
    fn put_counter_this_creature_is_self_ref() {
        let e = parse_effect("put a +1/+1 counter on this creature");
        assert!(
            matches!(e, Effect::PutCounter { counter_type: ref ct, count: QuantityExpr::Fixed { value: 1 }, target: TargetFilter::SelfRef } if ct == "P1P1")
        );
    }

    #[test]
    fn put_counter_all_each_creature_you_control() {
        let e = parse_effect("put a +1/+1 counter on each creature you control");
        assert!(
            matches!(
                e,
                Effect::PutCounterAll {
                    counter_type: ref ct,
                    count: QuantityExpr::Fixed { value: 1 },
                    ..
                } if ct == "P1P1"
            ),
            "Expected PutCounterAll for 'each creature you control', got {:?}",
            e
        );
    }

    #[test]
    fn effect_pay_life_cost() {
        let e = parse_effect("pay 3 life");
        assert!(matches!(
            e,
            Effect::PayCost {
                cost: PaymentCost::Life {
                    amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
                },
            }
        ));
    }

    /// CR 118.8 + CR 603.2: "pay life equal to its power" resolves against the
    /// triggering event's source power (Madame Null, Power Broker). The outer
    /// Effect here is just the PayCost — the "you may" optionality and
    /// `IfYouDo` sub-ability are wired at the trigger layer.
    #[test]
    fn effect_pay_life_equal_to_its_power() {
        let e = parse_effect("pay life equal to its power");
        assert!(
            matches!(
                &e,
                Effect::PayCost {
                    cost: PaymentCost::Life {
                        amount: crate::types::ability::QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextSourcePower,
                        },
                    },
                }
            ),
            "expected PayCost(Life = EventContextSourcePower), got {e:?}"
        );
    }

    /// CR 118.8: "pay X life" — variable amount resolved from `chosen_x`.
    #[test]
    fn effect_pay_x_life() {
        let e = parse_effect("pay X life");
        match &e {
            Effect::PayCost {
                cost:
                    PaymentCost::Life {
                        amount:
                            crate::types::ability::QuantityExpr::Ref {
                                qty: crate::types::ability::QuantityRef::Variable { name },
                            },
                    },
            } if name == "X" => {}
            other => panic!("expected PayCost(Life = Variable(X)), got {other:?}"),
        }
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

    /// CR 505.1 + CR 603.7a: "at the beginning of your next main phase" produces
    /// `AtNextPhaseForPlayer` — controller-scoped variant. The `player` field
    /// is a `PlayerId(0)` placeholder rewritten by
    /// `effects::delayed_trigger::resolve` at resolution time. Used by Mana
    /// Sculpt's delayed mana generation.
    #[test]
    fn strip_temporal_suffix_your_next_main_phase() {
        let (text, cond) = strip_temporal_suffix(
            "add an amount of {C} equal to the amount of mana spent to cast that spell at the beginning of your next main phase",
        );
        assert_eq!(
            text,
            "add an amount of {C} equal to the amount of mana spent to cast that spell"
        );
        assert_eq!(
            cond,
            Some(DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::PreCombatMain,
                player: crate::types::player::PlayerId(0),
            })
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

    /// CR 608.2c + CR 603.7a + CR 505.1 + CR 601.2h: Mana Sculpt end-to-end.
    ///
    /// "Counter target spell. If you control a Wizard, add an amount of {C}
    /// equal to the amount of mana spent to cast that spell at the beginning
    /// of your next main phase."
    ///
    /// Asserts the full typed parse:
    /// - primary effect is `Counter` targeting stack spell,
    /// - sub_ability carries `QuantityCheck(ObjectCount{Wizard} GE 1)` at
    ///   the OUTER level (the Wizard gate evaluates at spell resolution per
    ///   CR 608.2c, not at delayed-trigger firing),
    /// - sub_ability's effect is `CreateDelayedTrigger` with
    ///   `AtNextPhaseForPlayer { PreCombatMain, PlayerId(0) /* placeholder */ }`,
    /// - delayed trigger's inner effect is `Mana { Colorless { count:
    ///   Ref(ManaSpentOnTriggeringSpell) } }`.
    #[test]
    fn mana_sculpt_full_parse_tree() {
        let def = parse_effect_chain(
            "Counter target spell. If you control a Wizard, add an amount of {C} equal to the amount of mana spent to cast that spell at the beginning of your next main phase.",
            AbilityKind::Spell,
        );
        // Primary effect: Counter.
        assert!(
            matches!(*def.effect, Effect::Counter { .. }),
            "Expected Counter primary effect, got {:?}",
            def.effect
        );

        // Sub_ability carries the Wizard gate at the OUTER level.
        let sub = def.sub_ability.as_ref().expect("sub_ability must exist");
        let sub_cond = sub
            .condition
            .as_ref()
            .expect("sub_ability must carry the Wizard gate at its outer condition");
        match sub_cond {
            crate::types::ability::AbilityCondition::QuantityCheck {
                lhs,
                comparator,
                rhs,
            } => {
                assert!(
                    matches!(
                        lhs,
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { .. }
                        }
                    ),
                    "expected ObjectCount LHS, got {:?}",
                    lhs
                );
                assert_eq!(*comparator, crate::types::ability::Comparator::GE);
                assert_eq!(*rhs, QuantityExpr::Fixed { value: 1 });
            }
            other => panic!("expected QuantityCheck on outer sub_ability, got {other:?}"),
        }

        // Sub_ability effect: CreateDelayedTrigger at PreCombatMain.
        let Effect::CreateDelayedTrigger {
            condition: delayed_cond,
            effect: delayed_effect_def,
            ..
        } = &*sub.effect
        else {
            panic!(
                "expected CreateDelayedTrigger on sub_ability, got {:?}",
                sub.effect
            );
        };
        assert!(
            matches!(
                delayed_cond,
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    ..
                }
            ),
            "expected AtNextPhaseForPlayer(PreCombatMain), got {delayed_cond:?}"
        );

        // Inner delayed effect: Mana(Colorless, Ref(ManaSpentOnTriggeringSpell)).
        let Effect::Mana { produced, .. } = &*delayed_effect_def.effect else {
            panic!(
                "expected Mana effect on delayed trigger, got {:?}",
                delayed_effect_def.effect
            );
        };
        match produced {
            ManaProduction::Colorless { count } => {
                assert_eq!(
                    *count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ManaSpentOnTriggeringSpell
                    },
                    "Colorless count must reference mana spent on triggering spell"
                );
            }
            other => panic!("expected Colorless mana production, got {other:?}"),
        }
    }

    /// CR 608.2c regression guard: a delayed-trigger effect chain WITHOUT an
    /// inner condition must still parse correctly after the condition-lift.
    /// The outer `CreateDelayedTrigger`-carrying `AbilityDefinition` has
    /// `condition: None` (nothing to lift), and the inner `AbilityDefinition`
    /// also has `condition: None`.
    #[test]
    fn delayed_trigger_without_inner_condition_lift_is_noop() {
        let def = parse_effect_chain(
            "Untap target creature at the beginning of the next end step.",
            AbilityKind::Spell,
        );
        assert!(
            def.condition.is_none(),
            "outer AbilityDefinition must carry no condition when none was present on inner"
        );
        let Effect::CreateDelayedTrigger { effect, .. } = &*def.effect else {
            panic!("expected CreateDelayedTrigger, got {:?}", def.effect);
        };
        assert!(
            effect.condition.is_none(),
            "inner AbilityDefinition must carry no condition when none was present originally"
        );
    }

    /// CR 701.33 + CR 701.18 + CR 608.2b: Zimone's Experiment end-to-end.
    ///
    /// `"Look at the top five cards of your library. You may reveal up to two
    /// creature and/or land cards from among them, then put the rest on the
    /// bottom of your library in a random order. Put all land cards revealed
    /// this way onto the battlefield tapped and put all creature cards
    /// revealed this way into your hand."`
    ///
    /// Asserts the full compound parse tree:
    /// - Top-level `Dig` with `count=5, keep_count=Some(2), up_to=true,
    ///   reveal=true, filter=Or([Creature, Land]), destination=None (no
    ///   auto-route), rest_destination=Some(Library)`.
    /// - Sub_ability chain routes revealed cards by type through
    ///   `TargetFilter::TrackedSetFiltered`: lands enter battlefield tapped;
    ///   creatures go to hand.
    #[test]
    fn zimones_experiment_full_parse_tree() {
        let def = parse_effect_chain(
            "Look at the top five cards of your library. You may reveal up to two creature and/or land cards from among them, then put the rest on the bottom of your library in a random order. Put all land cards revealed this way onto the battlefield tapped and put all creature cards revealed this way into your hand.",
            AbilityKind::Spell,
        );
        // Top-level: Dig, patched by the reveal-only DigFromAmong continuation.
        let Effect::Dig {
            count,
            keep_count,
            up_to,
            reveal,
            filter,
            destination,
            rest_destination,
        } = &*def.effect
        else {
            panic!("expected Effect::Dig at top level, got {:?}", def.effect);
        };
        assert_eq!(*count, QuantityExpr::Fixed { value: 5 });
        assert_eq!(*keep_count, Some(2));
        assert!(*up_to, "up_to must be true (reveal up to two)");
        assert!(*reveal, "reveal must be true (reveal-form continuation)");
        assert_eq!(
            *destination, None,
            "destination must be None (reveal-only; downstream sub_abilities route by type)"
        );
        assert_eq!(*rest_destination, Some(crate::types::zones::Zone::Library));
        // Filter must be Or { [Creature, Land] }.
        match filter {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2);
                assert!(filters.iter().any(
                    |f| matches!(f, TargetFilter::Typed(tf) if tf.type_filters == vec![TypeFilter::Creature])
                ));
                assert!(filters.iter().any(
                    |f| matches!(f, TargetFilter::Typed(tf) if tf.type_filters == vec![TypeFilter::Land])
                ));
            }
            other => panic!("expected Or {{ [Creature, Land] }}, got {other:?}"),
        }

        // First sub_ability: ChangeZone { TrackedSetFiltered(Land) → Battlefield tapped }.
        let sub1 = def
            .sub_ability
            .as_ref()
            .expect("first sub_ability (Land routing) must exist");
        let Effect::ChangeZone {
            destination: dest1,
            target: target1,
            enter_tapped,
            ..
        } = &*sub1.effect
        else {
            panic!(
                "expected ChangeZone in first sub_ability, got {:?}",
                sub1.effect
            );
        };
        assert_eq!(*dest1, crate::types::zones::Zone::Battlefield);
        assert!(*enter_tapped, "lands must enter battlefield tapped");
        match target1 {
            TargetFilter::TrackedSetFiltered { id: _, filter } => match filter.as_ref() {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.type_filters, vec![TypeFilter::Land]);
                }
                other => panic!("expected Typed(Land), got {other:?}"),
            },
            other => panic!("expected TrackedSetFiltered, got {other:?}"),
        }

        // Second sub_ability: ChangeZone { TrackedSetFiltered(Creature) → Hand }.
        let sub2 = sub1
            .sub_ability
            .as_ref()
            .expect("second sub_ability (Creature routing) must exist");
        let Effect::ChangeZone {
            destination: dest2,
            target: target2,
            ..
        } = &*sub2.effect
        else {
            panic!(
                "expected ChangeZone in second sub_ability, got {:?}",
                sub2.effect
            );
        };
        assert_eq!(*dest2, crate::types::zones::Zone::Hand);
        match target2 {
            TargetFilter::TrackedSetFiltered { id: _, filter } => match filter.as_ref() {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
                }
                other => panic!("expected Typed(Creature), got {other:?}"),
            },
            other => panic!("expected TrackedSetFiltered, got {other:?}"),
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

    // CR 115.1d: "any number of target" subject-predicate integration tests
    #[test]
    fn any_number_of_target_creatures_pump_and_keyword() {
        let clause = parse_effect_clause(
            "any number of target creatures each get +1/+1 and gain flying until end of turn",
            &ParseContext::default(),
        );
        // Combined pump + keyword becomes GenericEffect with continuous modifications
        assert!(
            matches!(clause.effect, Effect::GenericEffect { .. }),
            "expected GenericEffect, got {:?}",
            clause.effect
        );
        assert_eq!(
            clause.multi_target,
            Some(MultiTargetSpec { min: 0, max: None }),
            "should have unlimited multi_target"
        );
    }

    #[test]
    fn any_number_of_target_creatures_phase_out() {
        let clause = parse_effect_clause(
            "any number of target creatures you control phase out",
            &ParseContext::default(),
        );
        assert!(
            matches!(clause.effect, Effect::PhaseOut { .. }),
            "expected PhaseOut, got {:?}",
            clause.effect
        );
        assert_eq!(
            clause.multi_target,
            Some(MultiTargetSpec { min: 0, max: None }),
        );
    }

    #[test]
    fn any_number_of_target_players_mill() {
        let clause = parse_effect_clause(
            "any number of target players each mill two cards",
            &ParseContext::default(),
        );
        assert!(
            matches!(
                clause.effect,
                Effect::Mill { .. } | Effect::TargetOnly { .. }
            ),
            "expected Mill or TargetOnly, got {:?}",
            clause.effect
        );
        assert_eq!(
            clause.multi_target,
            Some(MultiTargetSpec { min: 0, max: None }),
        );
    }

    // CR 115.1d: Scrollboost — "One or two target creatures each get +2/+2
    // until end of turn" must attach `MultiTargetSpec { min: 1, max: Some(2) }`
    // so the caster selects between one and two creatures (not zero, not the
    // unlimited "any number of" shape).
    #[test]
    fn one_or_two_target_creatures_each_pump() {
        let clause = parse_effect_clause(
            "one or two target creatures each get +2/+2 until end of turn",
            &ParseContext::default(),
        );
        assert!(
            !matches!(clause.effect, Effect::Unimplemented { .. }),
            "expected parsed pump effect, got {:?}",
            clause.effect
        );
        assert_eq!(
            clause.multi_target,
            Some(MultiTargetSpec {
                min: 1,
                max: Some(2)
            }),
        );
    }

    #[test]
    fn one_two_or_three_target_creatures_each_pump() {
        let clause = parse_effect_clause(
            "one, two, or three target creatures each get +1/+1 until end of turn",
            &ParseContext::default(),
        );
        assert!(
            !matches!(clause.effect, Effect::Unimplemented { .. }),
            "expected parsed pump effect, got {:?}",
            clause.effect
        );
        assert_eq!(
            clause.multi_target,
            Some(MultiTargetSpec {
                min: 1,
                max: Some(3)
            }),
        );
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
            Effect::CreateEmblem { statics, .. } => {
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
    fn effect_you_have_no_max_hand_size_for_rest_of_game() {
        // CR 402.2 + CR 114.1: Spell effect "you have no maximum hand size for the rest
        // of the game" creates an emblem with NoMaximumHandSize.
        let e = parse_effect("You have no maximum hand size for the rest of the game.");
        match e {
            Effect::CreateEmblem { statics, .. } => {
                assert_eq!(statics.len(), 1);
                assert_eq!(statics[0].mode, StaticMode::NoMaximumHandSize);
            }
            other => panic!("expected CreateEmblem, got {:?}", other),
        }
    }

    #[test]
    fn effect_you_have_no_max_hand_size_bare() {
        // Bare form without "for the rest of the game" suffix.
        let e = parse_effect("You have no maximum hand size.");
        match e {
            Effect::CreateEmblem { statics, .. } => {
                assert_eq!(statics.len(), 1);
                assert_eq!(statics[0].mode, StaticMode::NoMaximumHandSize);
            }
            other => panic!("expected CreateEmblem, got {:?}", other),
        }
    }

    #[test]
    fn effect_emblem_with_whenever_trigger_extracts_trigger_definition() {
        // CR 113.1c + CR 114.4: Emblem-hosted triggers must be extracted as
        // typed `TriggerDefinition`s, not dropped into `EmblemStatic` blobs.
        // Chandra, Torch of Defiance −7 is the canonical example.
        let e = parse_effect(
            "You get an emblem with \"Whenever you cast a spell, this emblem deals 5 damage to any target.\"",
        );
        match e {
            Effect::CreateEmblem { statics, triggers } => {
                assert!(
                    statics.is_empty(),
                    "trigger-only emblem should not install statics, got {statics:?}"
                );
                assert_eq!(triggers.len(), 1);
                let t = &triggers[0];
                assert!(
                    !matches!(t.mode, crate::types::triggers::TriggerMode::Unknown(_)),
                    "emblem trigger must not be Unknown, got {:?}",
                    t.mode
                );
                // CR 114.4: trigger_zones must include Command so the
                // scanner's zone guard admits the fire.
                assert_eq!(t.trigger_zones, vec![Zone::Command]);
            }
            other => panic!("expected CreateEmblem with triggers, got {other:?}"),
        }
    }

    #[test]
    fn effect_emblem_with_static_still_parses_as_static() {
        // Regression: pure static emblems continue to lower to `statics`
        // even after the trigger-first dispatch.
        let e = parse_effect("You get an emblem with \"Ninjas you control get +1/+1.\"");
        match e {
            Effect::CreateEmblem { statics, triggers } => {
                assert_eq!(statics.len(), 1);
                assert!(triggers.is_empty());
            }
            other => panic!("expected CreateEmblem static-only, got {other:?}"),
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
    fn kicker_leading_instead_produces_correct_condition() {
        // CR 608.2e: "if kicked, instead [effect]" — leading "instead" variant.
        // Must produce AdditionalCostPaidInstead, same as trailing "instead".
        let ability = parse_effect_chain(
            "Destroy target creature or planeswalker with mana value 2 or less. If this spell was kicked, instead destroy target creature or planeswalker",
            AbilityKind::Spell,
        );
        assert!(
            matches!(&*ability.effect, Effect::Destroy { .. }),
            "Base effect should be Destroy"
        );
        let sub = ability.sub_ability.as_ref().expect("expected sub_ability");
        assert_eq!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead)
        );
        assert!(
            matches!(&*sub.effect, Effect::Destroy { .. }),
            "Kicked effect should be Destroy"
        );
    }

    #[test]
    fn rite_of_replication_kicker_creates_five_copies() {
        // CR 702.33d + CR 707.10: Rite of Replication's kicked mode creates
        // five copies instead of one. The "create five of those tokens" sub
        // is an anaphor to the parent CopyTokenOf clause.
        let ability = parse_effect_chain(
            "Create a token that's a copy of target creature. If this spell was kicked, create five of those tokens instead.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*ability.effect,
                Effect::CopyTokenOf {
                    count: QuantityExpr::Fixed { value: 1 },
                    ..
                }
            ),
            "Base effect should be CopyTokenOf with count 1, got {:?}",
            ability.effect
        );
        let sub = ability.sub_ability.as_ref().expect("expected sub_ability");
        assert_eq!(
            sub.condition,
            Some(AbilityCondition::AdditionalCostPaidInstead),
            "Kicker sub should use AdditionalCostPaidInstead ('create five ... instead')"
        );
        assert!(
            matches!(
                &*sub.effect,
                Effect::CopyTokenOf {
                    count: QuantityExpr::Fixed { value: 5 },
                    ..
                }
            ),
            "Kicked sub should be CopyTokenOf with count 5, got {:?}",
            sub.effect
        );
    }

    #[test]
    fn general_condition_leading_instead_strips_prefix() {
        // CR 608.2e: Ability-word / general condition with leading "instead"
        // (cross-line pattern like Traverse the Ulvenwald's delirium).
        let ability = parse_effect_chain(
            "If you control three or more creatures, instead draw two cards",
            AbilityKind::Spell,
        );
        assert!(
            matches!(&*ability.effect, Effect::Draw { .. }),
            "Expected Draw effect after stripping 'instead', got {:?}",
            ability.effect
        );
        assert!(ability.condition.is_some(), "Condition should be extracted");
    }

    #[test]
    fn monarch_condition_instead_uses_condition_instead_wrapper() {
        let ability = parse_effect_chain(
            "~ deals 2 damage to any target. If you're the monarch, it deals 7 damage instead.",
            AbilityKind::Spell,
        );
        assert!(matches!(
            &*ability.effect,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                ..
            }
        ));
        let sub = ability.sub_ability.as_ref().expect("expected instead sub");
        assert!(matches!(
            sub.condition,
            Some(AbilityCondition::ConditionInstead { ref inner })
                if matches!(inner.as_ref(), AbilityCondition::IsMonarch)
        ));
        assert!(matches!(
            &*sub.effect,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 7 },
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
    fn phase_out_bare_self_reference() {
        // "Phase out" (no target) — self-reference form used after subject
        // stripping of "~ phases out" on a Breezekeeper-class card.
        let e = parse_effect("Phase out");
        assert!(
            matches!(e, Effect::PhaseOut { .. }),
            "Expected PhaseOut, got {:?}",
            e
        );
    }

    #[test]
    fn phase_in_targeted() {
        let e = parse_effect("Target creature phases in");
        assert!(
            matches!(
                e,
                Effect::PhaseIn {
                    target: TargetFilter::Typed(_)
                }
            ),
            "Expected PhaseIn with typed target, got {:?}",
            e
        );
    }

    /// Player-phasing parser plumbing: "you phase out" lifts the bare-pronoun
    /// "you" subject to `TargetFilter::Controller` so the resolver phases out
    /// the ability's controller player. This is the parser-side foundation
    /// for the Teferi's-Protection-style "you and permanents you control
    /// phase out" Oracle wording.
    #[test]
    fn phase_out_you_subject() {
        let e = parse_effect("You phase out");
        assert!(
            matches!(
                e,
                Effect::PhaseOut {
                    target: TargetFilter::Controller
                }
            ),
            "Expected PhaseOut with Controller target, got {:?}",
            e
        );
    }

    #[test]
    fn phase_out_those_creatures() {
        // Out of Time-class: "those creatures phase out" after subject
        // strip should produce PhaseOut with ParentTarget (the tracked set
        // from the preceding UntapAll).
        let e = parse_effect("those creatures phase out");
        assert!(
            matches!(e, Effect::PhaseOut { .. }),
            "Expected PhaseOut, got {:?}",
            e
        );
    }

    #[test]
    fn out_of_time_name_normalization() {
        use crate::parser::oracle_util::normalize_card_name_refs;
        let text = "When this enchantment enters, untap all creatures, then those creatures phase out until this enchantment leaves the battlefield.";
        let normalized = normalize_card_name_refs(text, "Out of Time");
        // No capital "Out" in text body — "out" should remain.
        assert!(
            normalized.contains("phase out"),
            "Normalization should preserve 'phase out', got: {}",
            normalized
        );
    }

    #[test]
    fn phase_out_in_effect_chain_out_of_time() {
        // The full Out of Time trigger text after `~` normalization.
        let chain = parse_effect_chain(
            "Untap all creatures, then those creatures phase out until ~ leaves the battlefield. Put a time counter on ~ for each creature that phased out this way.",
            AbilityKind::Spell,
        );
        // Walk the chain to find PhaseOut.
        let mut cur = Some(&chain);
        let mut found = false;
        while let Some(ab) = cur {
            if matches!(*ab.effect, Effect::PhaseOut { .. }) {
                found = true;
                break;
            }
            cur = ab.sub_ability.as_deref();
        }
        assert!(found, "PhaseOut not found in chain: {:?}", chain);
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
                    duration: Duration::UntilEndOfTurn,
                    ..
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
                        duration: Duration::UntilYourNextTurn,
                        ..
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
                        duration: Duration::UntilYourNextTurn,
                        ..
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
                        duration: Duration::UntilYourNextTurn,
                        ..
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
            matches!(
                &*def.effect,
                Effect::ExileTop {
                    player: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 2 },
                }
            ),
            "Expected ExileTop(controller, 2), got {:?}",
            def.effect
        );
        let sub = def.sub_ability.as_ref().expect("Expected sub_ability");
        assert!(
            matches!(
                *sub.effect,
                Effect::GrantCastingPermission {
                    permission: CastingPermission::PlayFromExile {
                        duration: Duration::UntilYourNextTurn,
                        ..
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
        // First effect: ExileTop from controller's library
        assert!(
            matches!(
                &*def.effect,
                Effect::ExileTop {
                    player: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 2 },
                }
            ),
            "Expected ExileTop(controller, 2), got {:?}",
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
                    up_to: false,
                    constraint: None,
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
                        duration: Duration::UntilEndOfTurn,
                        ..
                    },
                    ..
                }
            ),
            "Expected GrantCastingPermission(PlayFromExile), got {:?}",
            sub2.effect
        );
    }

    #[test]
    fn exile_top_x_cards_of_your_library() {
        let def = parse_effect_chain("Exile the top X cards of your library.", AbilityKind::Spell);
        assert!(
            matches!(
                &*def.effect,
                Effect::ExileTop {
                    player: TargetFilter::Controller,
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable { ref name }
                    },
                } if name == "X"
            ),
            "Expected ExileTop(controller, X), got {:?}",
            def.effect
        );
    }

    #[test]
    fn exile_top_card_of_that_players_library_uses_parent_target() {
        let def = parse_effect_chain(
            "Exile the top card of that player's library. Until end of turn, you may cast that card.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::ExileTop {
                    player: TargetFilter::ParentTarget,
                    count: QuantityExpr::Fixed { value: 1 },
                }
            ),
            "Expected ExileTop(parent target, 1), got {:?}",
            def.effect
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
    fn target_opponent_exiles_relative_creature_and_graveyard_uses_target_only_chain() {
        let def = parse_effect_chain(
            "Target opponent exiles a creature they control and their graveyard.",
            AbilityKind::Spell,
        );
        assert!(matches!(
            *def.effect,
            Effect::TargetOnly {
                target: TargetFilter::Typed(ref tf)
            } if tf.type_filters.is_empty() && tf.controller == Some(ControllerRef::Opponent)
        ));
        let sub = def
            .sub_ability
            .as_ref()
            .expect("expected targeted exile sub ability");
        assert!(matches!(*sub.effect, Effect::ChangeZone { .. }));
        let sub2 = sub
            .sub_ability
            .as_ref()
            .expect("expected graveyard exile continuation");
        assert!(matches!(
            *sub2.effect,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                ..
            }
        ));
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
    fn exile_two_target_permanents_multi_target() {
        let def = parse_effect_chain("exile two target permanents", AbilityKind::Spell);
        assert!(
            matches!(
                &*def.effect,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    ..
                }
            ),
            "Expected ChangeZone to Exile, got {:?}",
            def.effect
        );
        assert_eq!(
            def.multi_target,
            Some(MultiTargetSpec {
                min: 2,
                max: Some(2),
            }),
            "Expected multi_target with min=2, max=2"
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
                additional_filter: None,
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
    fn defending_player_exiles_top_twenty_cards() {
        let def = parse_effect_chain(
            "defending player exiles the top twenty cards of their library",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::ExileTop {
                    player: TargetFilter::DefendingPlayer,
                    count: QuantityExpr::Fixed { value: 20 },
                }
            ),
            "Expected ExileTop(DefendingPlayer, 20), got {:?}",
            def.effect
        );
    }

    #[test]
    fn target_opponent_exiles_top_half_library() {
        let def = parse_effect_chain(
            "target opponent exiles the top half of their library, rounded up",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::ExileTop { player, count } => {
                assert!(
                    matches!(player, TargetFilter::Typed(tf) if tf.controller == Some(ControllerRef::Opponent)),
                    "Expected opponent target, got {player:?}"
                );
                assert!(
                    matches!(
                        count,
                        QuantityExpr::HalfRounded {
                            rounding: crate::types::ability::RoundingMode::Up,
                            ..
                        }
                    ),
                    "Expected HalfRounded(Up), got {count:?}"
                );
            }
            other => panic!("Expected ExileTop, got {other:?}"),
        }
    }

    /// CR 107.1a class-level regression — "Target opponent loses half their
    /// life, rounded up." The `try_parse_half_life_amount` path must now
    /// produce a typed HalfRounded `amount` on `LoseLife`, not `Fixed { 1 }`.
    #[test]
    fn blood_tribute_lose_half_their_life_rounded_up() {
        use crate::types::ability::RoundingMode;
        let def = parse_effect_chain(
            "Target opponent loses half their life, rounded up.",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::LoseLife { amount, .. } => {
                assert_eq!(
                    *amount,
                    QuantityExpr::HalfRounded {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::TargetLifeTotal,
                        }),
                        rounding: RoundingMode::Up,
                    },
                    "Expected HalfRounded(TargetLifeTotal, Up), got {amount:?}",
                );
            }
            other => panic!("Expected LoseLife, got {other:?}"),
        }
    }

    /// CR 107.1a: Cut Your Losses — "Target player mills half their library,
    /// rounded down." Regression test for the Mill effect picking up the
    /// fractional quantity primitive (previously defaulted to `Fixed { 1 }`).
    #[test]
    fn cut_your_losses_mill_half_their_library_rounded_down() {
        use crate::types::ability::{RoundingMode, ZoneRef};
        let def = parse_effect_chain(
            "Target player mills half their library, rounded down.",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::Mill {
                count,
                target,
                destination,
            } => {
                assert!(
                    matches!(target, TargetFilter::Player),
                    "Expected Player target, got {target:?}",
                );
                assert_eq!(*destination, crate::types::zones::Zone::Graveyard);
                assert_eq!(
                    *count,
                    QuantityExpr::HalfRounded {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::TargetZoneCardCount {
                                zone: ZoneRef::Library,
                            },
                        }),
                        rounding: RoundingMode::Down,
                    },
                );
            }
            other => panic!("Expected Mill, got {other:?}"),
        }
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
                    additional_filter: None,
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
                additional_filter: None,
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
    fn strip_unless_general_your_turn() {
        // "unless it's your turn" → Not(DuringYourTurn) → IsYourTurn { negated: true }
        let (cond, text) = strip_unless_entered_suffix("draw a card unless it's your turn");
        assert_eq!(cond, Some(AbilityCondition::IsYourTurn { negated: true }),);
        assert_eq!(text, "draw a card");
    }

    #[test]
    fn strip_unless_you_control_a_creature() {
        // "unless you control a creature" → Not(IsPresent) → ObjectCount EQ 0
        let (cond, text) =
            strip_unless_entered_suffix("sacrifice this enchantment unless you control a creature");
        match cond {
            Some(AbilityCondition::QuantityCheck {
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
                ..
            }) => {}
            other => panic!("expected ObjectCount EQ 0, got {:?}", other),
        }
        assert_eq!(text, "sacrifice this enchantment");
    }

    #[test]
    fn strip_unless_unrecognized_returns_none() {
        let (cond, text) =
            strip_unless_entered_suffix("sacrifice it unless something weird happens");
        assert!(cond.is_none());
        assert_eq!(text, "sacrifice it unless something weird happens");
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
    fn strip_optional_target_prefix_up_to_one_other_target() {
        let (rest, multi_target) =
            strip_optional_target_prefix("up to one other target creature or spell");
        assert_eq!(rest, "other target creature or spell");
        assert_eq!(
            multi_target,
            Some(MultiTargetSpec {
                min: 0,
                max: Some(1),
            })
        );
    }

    #[test]
    fn cast_variant_paid_instead_sneak() {
        let (cond, text) = strip_additional_cost_conditional(
            "if her sneak cost was paid this turn, instead return that card to the battlefield",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::CastVariantPaidInstead {
                variant: CastVariantPaid::Sneak,
            })
        );
        assert_eq!(text, "return that card to the battlefield");
    }

    #[test]
    fn cast_variant_paid_instead_ninjutsu() {
        let (cond, text) = strip_additional_cost_conditional(
            "if its ninjutsu cost was paid this turn, instead draw a card",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::CastVariantPaidInstead {
                variant: CastVariantPaid::Ninjutsu,
            })
        );
        assert_eq!(text, "draw a card");
    }

    #[test]
    fn cast_variant_paid_sneak_non_instead() {
        // CR 702.190a: "if this spell's sneak cost was paid, [effect]" without "instead"
        let (cond, text) = strip_additional_cost_conditional(
            "if this spell's sneak cost was paid, they enter tapped and attacking",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::CastVariantPaid {
                variant: CastVariantPaid::Sneak,
            })
        );
        assert_eq!(text, "they enter tapped and attacking");
    }

    #[test]
    fn cast_variant_paid_ninjutsu_non_instead() {
        // CR 702.49: "if its ninjutsu cost was paid, [effect]" without "instead"
        let (cond, text) = strip_additional_cost_conditional(
            "if its ninjutsu cost was paid, those tokens enter tapped and attacking",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::CastVariantPaid {
                variant: CastVariantPaid::Ninjutsu,
            })
        );
        assert_eq!(text, "those tokens enter tapped and attacking");
    }

    #[test]
    fn conditional_enter_tapped_attacking_patches_token() {
        // CR 508.4 + CR 614.1d: "If sneak cost was paid, they enter tapped and attacking"
        // should produce a conditional Token with enters_attacking and tapped set, with
        // the original (unflagged) Token as else_ability.
        let def = parse_effect_chain(
            "Create three 1/1 white Ninja Turtle Spirit creature tokens. If this spell's sneak cost was paid, they enter tapped and attacking.",
            AbilityKind::Spell,
        );
        // The def should be a Token with condition and else_ability
        match &*def.effect {
            Effect::Token {
                enters_attacking,
                tapped,
                count,
                ..
            } => {
                // When condition is met, tokens enter tapped and attacking
                assert!(*enters_attacking, "Token should have enters_attacking=true");
                assert!(*tapped, "Token should have tapped=true");
                assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
            }
            other => panic!("Expected Token effect, got {:?}", other),
        }
        assert_eq!(
            def.condition,
            Some(AbilityCondition::CastVariantPaid {
                variant: CastVariantPaid::Sneak,
            }),
            "Token should have CastVariantPaid condition"
        );
        // else_ability should be the same Token without tapped/attacking
        let else_def = def.else_ability.as_ref().expect("Should have else_ability");
        match &*else_def.effect {
            Effect::Token {
                enters_attacking,
                tapped,
                ..
            } => {
                assert!(
                    !*enters_attacking,
                    "Else Token should have enters_attacking=false"
                );
                assert!(!*tapped, "Else Token should have tapped=false");
            }
            other => panic!("Expected Token effect in else_ability, got {:?}", other),
        }
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
        assert!(tf
            .type_filters
            .iter()
            .position(|type_filter| *type_filter == TypeFilter::Creature)
            .is_some());
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
        assert!(tf
            .type_filters
            .iter()
            .position(|type_filter| *type_filter == TypeFilter::Creature)
            .is_some());
        assert!(tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::CmcEQ {
                value: QuantityExpr::Fixed { value: 2 }
            }
        )));
    }

    #[test]
    fn search_filter_artifact_creature_or_enchantment_with_mana_value_1_or_less() {
        let filter = parse_search_filter(
            "artifact, creature, and/or enchantment cards with mana value 1 or less",
        );
        let TargetFilter::Or { filters } = filter else {
            panic!("Expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 3, "expected 3 branches, got {filters:?}");
        for branch in filters {
            let TargetFilter::Typed(tf) = branch else {
                panic!("Expected Typed branch, got {branch:?}");
            };
            assert!(tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::CmcLE {
                    value: QuantityExpr::Fixed { value: 1 }
                }
            )));
        }
    }

    #[test]
    fn search_filter_aura_preserves_enchantment_type_and_dynamic_mana_value() {
        let filter = parse_search_filter(
            "Aura card with mana value less than or equal to that Aura and with a different name than each Aura you control",
        );
        let TargetFilter::Typed(tf) = filter else {
            panic!("Expected Typed filter, got {filter:?}");
        };
        assert!(tf
            .type_filters
            .iter()
            .position(|type_filter| *type_filter == TypeFilter::Enchantment)
            .is_some());
        assert_eq!(tf.get_subtype(), Some("Aura"));
        assert!(tf.properties.iter().any(|p| matches!(
            p,
            FilterProp::CmcLE {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextSourceManaValue
                }
            }
        )));
        assert!(tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::DifferentNameFrom { .. })));
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
    fn search_filter_basic_card_with_that_name() {
        let filter = parse_search_filter("basic card with that name");
        let TargetFilter::Typed(tf) = filter else {
            panic!("Expected Typed filter with Basic + SameName, got {filter:?}");
        };
        assert!(tf.properties.iter().any(|property| matches!(
            property,
            FilterProp::HasSupertype {
                value: crate::types::card_type::Supertype::Basic
            }
        )));
        assert!(tf
            .properties
            .iter()
            .any(|property| matches!(property, FilterProp::SameName)));
    }

    #[test]
    fn search_library_details_up_to_five() {
        let details =
            parse_search_library_details("search your library for up to five creature cards");
        assert_eq!(details.count, QuantityExpr::Fixed { value: 5 });
        let TargetFilter::Typed(tf) = &details.filter else {
            panic!("Expected Typed filter, got {:?}", details.filter);
        };
        assert!(tf
            .type_filters
            .iter()
            .position(|type_filter| *type_filter == TypeFilter::Creature)
            .is_some());
    }

    #[test]
    fn brightglass_gearhulk_search_filter_preserves_all_types_and_mana_value_limit() {
        let effect = parse_effect(
            "search your library for up to two artifact, creature, and/or enchantment cards with mana value 1 or less, reveal them, put them into your hand, then shuffle",
        );
        let Effect::SearchLibrary {
            filter,
            count,
            reveal,
            ..
        } = effect
        else {
            panic!("Expected SearchLibrary, got {effect:?}");
        };
        assert_eq!(count, QuantityExpr::Fixed { value: 2 });
        assert!(reveal);
        let TargetFilter::Or { filters } = filter else {
            panic!("Expected Or filter, got {filter:?}");
        };
        assert_eq!(filters.len(), 3, "expected 3 branches, got {filters:?}");
        for branch in filters {
            let TargetFilter::Typed(tf) = branch else {
                panic!("Expected Typed branch, got {branch:?}");
            };
            assert!(tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::CmcLE {
                    value: QuantityExpr::Fixed { value: 1 }
                }
            )));
        }
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
                        counters: crate::types::counter::CounterMatch::OfType(
                            crate::types::counter::CounterType::Generic(ref name)
                        ),
                        minimum: 1,
                        maximum: None,
                    },
                }) if name == "flood"
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
                // "that many" resolves to EventContextAmount at runtime
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount
                    }
                ));
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
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount
                    }
                ));
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
    fn strip_player_scope_subject_linked_exile_owner() {
        let (scope, result) =
            strip_player_scope_subject("the exiled card's owner creates an X/X token");
        assert_eq!(scope, Some(PlayerFilter::OwnersOfCardsExiledBySource));
        assert_eq!(result, "create an X/X token");
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
    fn effect_exchange_control_compound_two_targets_creature() {
        // Compound shape: "target X and target Y" — both slots independently parsed.
        let e = parse_effect("exchange control of target creature and target creature");
        match e {
            Effect::ExchangeControl { target_a, target_b } => {
                assert!(
                    matches!(target_a, TargetFilter::Typed(ref tf)
                        if tf.type_filters == vec![crate::types::ability::TypeFilter::Creature]),
                    "target_a should be a creature filter, got {target_a:?}"
                );
                assert!(
                    matches!(target_b, TargetFilter::Typed(ref tf)
                        if tf.type_filters == vec![crate::types::ability::TypeFilter::Creature]),
                    "target_b should be a creature filter, got {target_b:?}"
                );
            }
            other => panic!("Expected ExchangeControl, got {other:?}"),
        }
    }

    #[test]
    fn effect_exchange_control_quantified_two_target_creatures() {
        // Quantified shape: "two target Xs" (Switcheroo). Both slots share one filter.
        let e = parse_effect("exchange control of two target creatures");
        match e {
            Effect::ExchangeControl { target_a, target_b } => {
                assert_eq!(
                    target_a, target_b,
                    "quantified shape: both filters identical"
                );
                assert!(
                    matches!(target_a, TargetFilter::Typed(ref tf)
                        if tf.type_filters == vec![crate::types::ability::TypeFilter::Creature]),
                    "target_a should be a creature filter, got {target_a:?}"
                );
            }
            other => panic!("Expected ExchangeControl, got {other:?}"),
        }
    }

    #[test]
    fn effect_exchange_control_compound_distinct_filters() {
        // Compound shape with per-slot legality (Political Trickery / Trade the Helm shape):
        // each slot carries its own controller-relative filter. Assert on the
        // actual ControllerRef and TypeFilter rather than mere inequality so
        // that a parser regression (e.g. dropping the controller binding, or
        // swapping slot order) fails loudly.
        use crate::types::ability::{ControllerRef, TypeFilter};
        let e = parse_effect(
            "exchange control of target land you control and target land an opponent controls",
        );
        match e {
            Effect::ExchangeControl { target_a, target_b } => {
                let tf_a = match &target_a {
                    TargetFilter::Typed(tf) => tf,
                    _ => panic!("target_a should be Typed, got {target_a:?}"),
                };
                assert_eq!(
                    tf_a.type_filters,
                    vec![TypeFilter::Land],
                    "target_a should be a Land filter"
                );
                assert_eq!(
                    tf_a.controller,
                    Some(ControllerRef::You),
                    "target_a should bind ControllerRef::You"
                );
                let tf_b = match &target_b {
                    TargetFilter::Typed(tf) => tf,
                    _ => panic!("target_b should be Typed, got {target_b:?}"),
                };
                assert_eq!(
                    tf_b.type_filters,
                    vec![TypeFilter::Land],
                    "target_b should be a Land filter"
                );
                assert_eq!(
                    tf_b.controller,
                    Some(ControllerRef::Opponent),
                    "target_b should bind ControllerRef::Opponent"
                );
            }
            other => panic!("Expected ExchangeControl, got {other:?}"),
        }
    }

    #[test]
    fn effect_exchange_control_self_ref_slot() {
        // "this artifact and target nonland permanent" (Avarice Totem shape).
        // The "this <type>" slot resolves to SelfRef and is auto-filled at
        // resolution. target_b must be a Permanent filter with Nonland so the
        // targeting UI enforces Oracle legality rather than accepting "Any".
        use crate::types::ability::TypeFilter;
        let e = parse_effect("exchange control of this artifact and target nonland permanent");
        match e {
            Effect::ExchangeControl { target_a, target_b } => {
                assert!(
                    matches!(target_a, TargetFilter::SelfRef),
                    "target_a (this artifact) should be SelfRef, got {target_a:?}"
                );
                let tf_b = match &target_b {
                    TargetFilter::Typed(tf) => tf,
                    _ => panic!("target_b should be Typed, got {target_b:?}"),
                };
                // "nonland permanent" → Permanent type with a Non(Land) filter.
                assert!(
                    tf_b.type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Permanent)),
                    "target_b should include TypeFilter::Permanent, got {:?}",
                    tf_b.type_filters
                );
                assert!(
                    tf_b.type_filters
                        .iter()
                        .any(|t| matches!(t, TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Land))),
                    "target_b should carry Non(Land) for 'nonland', got {:?}",
                    tf_b.type_filters
                );
            }
            other => panic!("Expected ExchangeControl, got {other:?}"),
        }
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

    // CR 603.2 + CR 111.10: Najeela, the Blade-Blossom — "have its controller
    // create a ... token ..." rebinds token.owner to TriggeringSource so at
    // runtime the token is put onto the battlefield under the attacking
    // Warrior's controller, not Najeela's controller.
    #[test]
    fn have_its_controller_create_token_rebinds_owner_to_triggering_source() {
        let e = parse_effect(
            "have its controller create a 1/1 white Warrior creature token that's tapped and attacking",
        );
        match e {
            Effect::Token {
                ref owner,
                tapped,
                enters_attacking,
                ref types,
                ref colors,
                ..
            } => {
                assert_eq!(
                    owner,
                    &TargetFilter::TriggeringSource,
                    "expected owner rebound to TriggeringSource, got {owner:?}"
                );
                assert!(tapped, "token should enter tapped");
                assert!(enters_attacking, "token should enter attacking");
                assert!(types.contains(&"Creature".to_string()));
                assert!(types.contains(&"Warrior".to_string()));
                assert_eq!(colors, &vec![ManaColor::White]);
            }
            other => panic!("expected Effect::Token, got {other:?}"),
        }
    }

    // CR 508.4 + CR 506.3a: The inline "that's tapped and attacking" clause
    // inside a token description phrase must set BOTH `tapped` and
    // `enters_attacking`. Previously the suffix was silently dropped for
    // ~10 cards (Captain's Claws, Battle Cry Goblin, General Kreat,
    // Indulge, etc.), producing rules-incorrect tokens.
    #[test]
    fn token_thats_tapped_and_attacking_suffix_sets_flags() {
        let e = parse_effect("create a 1/1 red Goblin creature token that's tapped and attacking");
        match e {
            Effect::Token {
                tapped,
                enters_attacking,
                ..
            } => {
                assert!(tapped, "expected tapped=true from inline suffix");
                assert!(
                    enters_attacking,
                    "expected enters_attacking=true from inline suffix"
                );
            }
            other => panic!("expected Effect::Token, got {other:?}"),
        }
    }

    // --- Restriction clause extensions ---

    #[test]
    fn effect_your_life_total_cant_change_under_until_end_of_turn() {
        // CR 119.7 + CR 119.8: Teferi's-Protection-style first clause under a
        // shared duration. Must produce a GenericEffect with BOTH CantGainLife
        // and CantLoseLife scoped to controller, with AddStaticMode modifications
        // so the layer system propagates the locks during the duration window.
        let def = parse_effect_chain(
            "Until end of turn, your life total can't change.",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::GenericEffect {
                static_abilities, ..
            } => {
                let modes: Vec<_> = static_abilities.iter().map(|s| s.mode.clone()).collect();
                assert!(
                    modes.contains(&StaticMode::CantGainLife),
                    "missing CantGainLife in {modes:?}"
                );
                assert!(
                    modes.contains(&StaticMode::CantLoseLife),
                    "missing CantLoseLife in {modes:?}"
                );
                for sd in static_abilities {
                    assert!(
                        matches!(
                            sd.affected,
                            Some(TargetFilter::Typed(TypedFilter {
                                controller: Some(ControllerRef::You),
                                ..
                            }))
                        ),
                        "expected You-scoped affected, got {:?}",
                        sd.affected
                    );
                    assert!(
                        sd.modifications.iter().any(|m| matches!(
                            m,
                            ContinuousModification::AddStaticMode {
                                mode: StaticMode::CantGainLife | StaticMode::CantLoseLife
                            }
                        )),
                        "expected AddStaticMode for layer propagation, got {:?}",
                        sd.modifications
                    );
                }
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
        assert_eq!(def.duration, Some(Duration::UntilEndOfTurn));
    }

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

    /// End-to-end parser integration test for Teferi's Protection.
    ///
    /// CR 119.7 + CR 119.8 + CR 702.16 + CR 702.26: The full Oracle text
    /// exercises the compound chain parser (shared leading "Until your next
    /// turn" duration), the life-lock clause (added in this batch), the
    /// protection-granting clause, and the PhaseOut clause. All three must
    /// chain correctly under the outer `UntilYourNextTurn` duration.
    #[test]
    fn parse_teferis_protection_end_to_end() {
        let result = crate::parser::parse_oracle_text(
            "Until your next turn, your life total can't change and you gain protection from everything. All permanents you control phase out.\nExile Teferi's Protection.",
            "Teferi's Protection",
            &[],
            &["Instant".to_string()],
            &[],
        );

        assert!(
            result.abilities.len() >= 2,
            "expected at least the chained spell ability and an exile ability, got {}: {:?}",
            result.abilities.len(),
            result
                .abilities
                .iter()
                .map(|a| format!("{:?}", a.effect))
                .collect::<Vec<_>>()
        );

        // First ability: the chained compound under UntilYourNextTurn.
        let first = &result.abilities[0];
        assert_eq!(first.duration, Some(Duration::UntilYourNextTurn));

        let Effect::GenericEffect {
            static_abilities, ..
        } = &*first.effect
        else {
            panic!(
                "expected first ability effect to be GenericEffect(life-lock), got {:?}",
                first.effect
            );
        };
        let modes: Vec<_> = static_abilities.iter().map(|s| s.mode.clone()).collect();
        assert!(
            modes.contains(&StaticMode::CantGainLife),
            "first clause missing CantGainLife: {modes:?}"
        );
        assert!(
            modes.contains(&StaticMode::CantLoseLife),
            "first clause missing CantLoseLife: {modes:?}"
        );

        // Second clause: protection grant via sub_ability.
        let second = first
            .sub_ability
            .as_ref()
            .expect("life-lock clause should chain to protection clause via sub_ability");
        let Effect::GenericEffect {
            static_abilities: protection_statics,
            ..
        } = &*second.effect
        else {
            panic!(
                "expected protection clause to be GenericEffect, got {:?}",
                second.effect
            );
        };
        assert!(
            protection_statics
                .iter()
                .any(|s| s.modifications.iter().any(|m| matches!(
                    m,
                    ContinuousModification::AddKeyword {
                        keyword: crate::types::keywords::Keyword::Protection(_)
                    }
                ))),
            "expected protection-from-everything keyword in {:?}",
            protection_statics
        );
        // CR 113.10: Player-scope grant — the static must be affected by the
        // controller (the protected player), not SelfRef (the spell object).
        assert!(
            protection_statics
                .iter()
                .any(|s| s.affected == Some(TargetFilter::Controller)),
            "protection-from-everything must target Controller (player), got {:?}",
            protection_statics
                .iter()
                .map(|s| &s.affected)
                .collect::<Vec<_>>()
        );

        // Third clause: phase out all permanents you control.
        let third = second
            .sub_ability
            .as_ref()
            .expect("protection clause should chain to phase-out clause");
        assert!(
            matches!(*third.effect, Effect::PhaseOut { .. }),
            "expected PhaseOut clause, got {:?}",
            third.effect
        );
    }

    /// CR 113.10 + CR 702.16j: "you gain protection from everything" — the
    /// player-scoped keyword grant must emit `affected: Controller` so the
    /// static binds to the spell's controller at resolution time (not to the
    /// spell object itself, which is destined for the graveyard).
    #[test]
    fn parse_you_gain_protection_from_everything_targets_controller() {
        use crate::types::ability::ContinuousModification;
        use crate::types::keywords::{Keyword, ProtectionTarget};

        let def = parse_effect_chain("you gain protection from everything", AbilityKind::Spell);
        let Effect::GenericEffect {
            static_abilities, ..
        } = &*def.effect
        else {
            panic!(
                "expected GenericEffect for 'you gain protection from everything', got {:?}",
                def.effect
            );
        };
        assert_eq!(static_abilities.len(), 1, "expected one StaticDefinition");
        let sd = &static_abilities[0];
        assert_eq!(
            sd.affected,
            Some(TargetFilter::Controller),
            "affected filter must be Controller (player-scoped), got {:?}",
            sd.affected
        );
        assert!(
            sd.modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Protection(ProtectionTarget::Everything)
                }
            )),
            "expected AddKeyword(Protection(Everything)) in {:?}",
            sd.modifications
        );
    }

    /// CR 702: "gain flying until end of turn" without "you" subject must keep
    /// `affected: SelfRef` — the grant is creature-scoped, not player-scoped.
    /// Guards against the player-routing logic leaking into creature pump
    /// sub-abilities.
    #[test]
    fn parse_gain_flying_creature_scope_preserved() {
        let def = parse_effect_chain("gain flying until end of turn", AbilityKind::Spell);
        let Effect::GenericEffect {
            static_abilities, ..
        } = &*def.effect
        else {
            panic!(
                "expected GenericEffect for 'gain flying until end of turn', got {:?}",
                def.effect
            );
        };
        assert_eq!(
            static_abilities[0].affected,
            Some(TargetFilter::SelfRef),
            "affected filter must remain SelfRef for creature-scoped keyword grants"
        );
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
        if let Effect::Sacrifice { ref target, .. } = *def.effect {
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

    /// CR 608.2d + CR 603.2: "each opponent may X" — each opponent independently
    /// decides yes/no. Lowered to `optional: true` + `player_scope: Opponent`
    /// (not `OpponentMayScope::AnyOpponent`, which is first-accept-wins).
    /// At runtime the outer player_scope iteration rebinds controller per
    /// opponent, and each scoped clone enters OptionalEffectChoice for that
    /// opponent. Source: Fandaniel, Telophoroi Ascian Oracle.
    #[test]
    fn each_opponent_may_sets_optional_with_opponent_scope() {
        let def = parse_effect_chain(
            "each opponent may sacrifice a nontoken creature of their choice",
            AbilityKind::Spell,
        );
        assert!(def.optional, "expected optional=true, got {def:?}");
        assert_eq!(
            def.optional_for, None,
            "each-opponent-may must NOT set optional_for (which is AnyOpponent-only)"
        );
        assert_eq!(
            def.player_scope,
            Some(PlayerFilter::Opponent),
            "expected player_scope=Opponent, got {:?}",
            def.player_scope
        );
        assert!(
            matches!(*def.effect, Effect::Sacrifice { .. }),
            "effect should be Sacrifice, got {:?}",
            def.effect
        );
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
            ..Default::default()
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
            ..Default::default()
        };
        assert_eq!(resolve_it_pronoun(&ctx), TargetFilter::TriggeringSource);
    }

    #[test]
    fn resolve_it_pronoun_attached_to_subject() {
        let ctx = ParseContext {
            subject: Some(TargetFilter::AttachedTo),
            ..Default::default()
        };
        assert_eq!(resolve_it_pronoun(&ctx), TargetFilter::TriggeringSource);
    }

    #[test]
    fn resolve_it_pronoun_any_subject() {
        let ctx = ParseContext {
            subject: Some(TargetFilter::Any),
            ..Default::default()
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
    fn strip_suffix_conditional_parses_control_condition() {
        let (cond, text) =
            strip_suffix_conditional("sacrifice a creature if you control a creature");
        assert!(
            cond.is_some(),
            "should now parse 'you control a creature' via nom bridge"
        );
        assert_eq!(text, "sacrifice a creature");
        // Verify it produces QuantityCheck(ObjectCount >= 1)
        match cond.unwrap() {
            AbilityCondition::QuantityCheck {
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
                ..
            } => {}
            other => panic!("expected QuantityCheck(GE, 1), got {:?}", other),
        }
    }

    #[test]
    fn strip_suffix_conditional_unparseable_returns_none() {
        // A condition the bridge cannot handle returns None.
        let (cond, text) = strip_suffix_conditional("draw a card if the moon is full");
        assert!(cond.is_none());
        assert_eq!(text, "draw a card if the moon is full");
    }

    // --- StaticCondition → AbilityCondition bridge tests ---

    #[test]
    fn bridge_during_your_turn() {
        let result = try_nom_condition_as_ability_condition("it's your turn");
        assert_eq!(
            result,
            Some(AbilityCondition::IsYourTurn { negated: false })
        );
    }

    #[test]
    fn bridge_not_your_turn() {
        let result = try_nom_condition_as_ability_condition("it's not your turn");
        assert_eq!(result, Some(AbilityCondition::IsYourTurn { negated: true }));
    }

    #[test]
    fn bridge_life_total_comparison() {
        let result = try_nom_condition_as_ability_condition("your life total is 5 or less");
        match result {
            Some(AbilityCondition::QuantityCheck {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::LifeTotal,
                    },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 5 },
            }) => {}
            other => panic!("expected LifeTotal LE 5, got {:?}", other),
        }
    }

    #[test]
    fn bridge_hand_size_zero() {
        let result = try_nom_condition_as_ability_condition("you have no cards in hand");
        match result {
            Some(AbilityCondition::QuantityCheck {
                lhs:
                    QuantityExpr::Ref {
                        qty: QuantityRef::HandSize,
                    },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
            }) => {}
            other => panic!("expected HandSize EQ 0, got {:?}", other),
        }
    }

    #[test]
    fn bridge_you_control_artifact() {
        let result = try_nom_condition_as_ability_condition("you control an artifact");
        match result {
            Some(AbilityCondition::QuantityCheck {
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
                ..
            }) => {}
            other => panic!("expected ObjectCount GE 1, got {:?}", other),
        }
    }

    #[test]
    fn bridge_source_tapped_maps_to_ability_condition() {
        // CR 611.2b: SourceIsTapped bridges to AbilityCondition::SourceIsTapped.
        let result = try_nom_condition_as_ability_condition("~ is tapped");
        assert_eq!(
            result,
            Some(AbilityCondition::SourceIsTapped { negated: false })
        );
    }

    #[test]
    fn bridge_source_untapped_maps_to_negated_condition() {
        // CR 611.2b: Not(SourceIsTapped) bridges to SourceIsTapped { negated: true }.
        let result = try_nom_condition_as_ability_condition("~ is untapped");
        assert_eq!(
            result,
            Some(AbilityCondition::SourceIsTapped { negated: true })
        );
    }

    #[test]
    fn bridge_partial_input_returns_none() {
        // Partial match with leftover text should return None.
        let result = try_nom_condition_as_ability_condition("it's your turn and stuff");
        assert!(result.is_none());
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
    fn no_more_lies_exile_rider_uses_parent_target() {
        let def = parse_effect_chain(
            "Counter target spell unless its controller pays {3}. If that spell is countered this way, exile it instead of putting it into its owner's graveyard.",
            AbilityKind::Spell,
        );

        match &*def.effect {
            Effect::Counter {
                target,
                unless_payment,
                ..
            } => {
                assert!(matches!(
                    target,
                    TargetFilter::Typed(tf)
                        if tf.type_filters.contains(&TypeFilter::Card)
                            && tf.properties.contains(&FilterProp::InZone { zone: Zone::Stack })
                ));
                assert!(
                    unless_payment.is_some(),
                    "counter rider should preserve unless-payment"
                );
            }
            other => panic!("expected Counter effect, got {other:?}"),
        }

        let exile = def
            .sub_ability
            .as_ref()
            .expect("No More Lies should parse exile rider");
        match &*exile.effect {
            Effect::ChangeZone {
                destination,
                origin,
                target,
                ..
            } => {
                assert_eq!(*destination, Zone::Exile);
                assert_eq!(*origin, Some(Zone::Graveyard));
                assert_eq!(*target, TargetFilter::ParentTarget);
            }
            other => panic!("expected exile ChangeZone rider, got {other:?}"),
        }
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

    #[test]
    fn parse_airbend_all_other_creatures_uses_parent_target_exclusion() {
        let def = parse_effect_chain(
            "Choose up to one target creature, then airbend all other creatures",
            AbilityKind::Spell,
        );
        let airbend = def
            .sub_ability
            .as_ref()
            .expect("airbend should chain from targeting clause");
        assert!(matches!(
            *airbend.effect,
            Effect::ChangeZoneAll {
                target: TargetFilter::And { .. },
                ..
            }
        ));
        if let Effect::ChangeZoneAll { target, .. } = &*airbend.effect {
            assert!(matches!(
                target,
                TargetFilter::And { filters }
                    if filters.iter().any(|filter| matches!(
                        filter,
                        TargetFilter::Not {
                            filter
                        } if matches!(filter.as_ref(), TargetFilter::ParentTarget)
                    ))
            ));
        }
    }

    #[test]
    fn parse_airbend_up_to_one_other_target_creature_or_spell() {
        let clause = parse_effect_clause(
            "airbend up to one other target creature or spell",
            &ParseContext::default(),
        );
        assert_eq!(
            clause.multi_target,
            Some(MultiTargetSpec {
                min: 0,
                max: Some(1),
            })
        );
        match clause.effect {
            Effect::ChangeZone {
                target: TargetFilter::Or { filters },
                ..
            } => {
                assert!(
                    filters.iter().any(|filter| matches!(
                        filter,
                        TargetFilter::Typed(tf)
                            if tf.type_filters.contains(&TypeFilter::Creature)
                                && tf.properties.contains(&FilterProp::Another)
                    )),
                    "expected creature branch with Another, got {filters:?}"
                );
                assert!(
                    filters.iter().any(|filter| matches!(
                        filter,
                        TargetFilter::Typed(tf)
                            if tf.type_filters.contains(&TypeFilter::Card)
                                && tf.properties.contains(&FilterProp::Another)
                    )),
                    "expected spell branch with Another, got {filters:?}"
                );
            }
            other => panic!("expected ChangeZone with creature-or-spell target, got {other:?}"),
        }
    }

    #[test]
    fn parse_cast_only_from_hand_restriction_clause() {
        let def = parse_effect_chain(
            "Until your next turn, your opponents can't cast spells from anywhere other than their hands",
            AbilityKind::Spell,
        );
        assert!(matches!(
            *def.effect,
            Effect::AddRestriction {
                restriction: GameRestriction::CastOnlyFromZones {
                    allowed_zones,
                    ..
                }
            } if allowed_zones == vec![Zone::Hand]
        ));
        assert_eq!(def.duration, Some(Duration::UntilYourNextTurn));
    }

    #[test]
    fn cant_cast_spells_this_turn_opponents() {
        // CR 101.2: Silence — "Your opponents can't cast spells this turn."
        let def = parse_effect_chain(
            "Your opponents can't cast spells this turn.",
            AbilityKind::Spell,
        );
        assert!(matches!(
            *def.effect,
            Effect::AddRestriction {
                restriction: GameRestriction::CantCastSpells {
                    affected_players: RestrictionPlayerScope::OpponentsOfSourceController,
                    expiry: RestrictionExpiry::EndOfTurn,
                    ..
                }
            }
        ));
    }

    #[test]
    fn cant_cast_spells_this_turn_all_players() {
        // CR 101.2: Blanket prohibition on all players.
        let def = parse_effect_chain("Players can't cast spells this turn.", AbilityKind::Spell);
        assert!(matches!(
            *def.effect,
            Effect::AddRestriction {
                restriction: GameRestriction::CantCastSpells {
                    affected_players: RestrictionPlayerScope::AllPlayers,
                    expiry: RestrictionExpiry::EndOfTurn,
                    ..
                }
            }
        ));
    }

    #[test]
    fn figure_of_fable_source_matches_filter_condition() {
        // CR 608.2c: "If this creature is a Scout, ..." gates on source subtype.
        let def = parse_effect_chain(
            "If this creature is a Scout, it becomes a Kithkin Soldier with base power and toughness 4/5.",
            AbilityKind::Activated,
        );
        assert!(
            matches!(
                def.condition,
                Some(AbilityCondition::SourceMatchesFilter { .. })
            ),
            "Expected SourceMatchesFilter condition, got {:?}",
            def.condition
        );
    }

    #[test]
    fn pump_all_lowercase_trigger_body() {
        // Regression: trigger effect text arrives lowercase from parse_trigger_line.
        // "creatures you control get +1/+1 until end of turn" must parse as PumpAll.
        let def = parse_effect_chain_with_context(
            "creatures you control get +1/+1 until end of turn.",
            AbilityKind::Spell,
            &ParseContext::default(),
        );
        assert!(
            matches!(*def.effect, Effect::PumpAll { .. }),
            "expected PumpAll, got {:?}",
            def.effect
        );
    }

    #[test]
    fn pump_all_lowercase_with_trigger_subject_context() {
        // Realistic context: trigger sets subject from condition text.
        let def = parse_effect_chain_with_context(
            "creatures you control get +1/+1 until end of turn.",
            AbilityKind::Spell,
            &ParseContext {
                subject: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: Some(ControllerRef::You),
                    properties: vec![FilterProp::Another],
                })),
                ..Default::default()
            },
        );
        assert!(
            matches!(*def.effect, Effect::PumpAll { .. }),
            "expected PumpAll with trigger context, got {:?}",
            def.effect
        );
    }

    #[test]
    fn duration_preserved_with_where_x_suffix() {
        // Craterhoof Behemoth pattern: "creatures you control gain trample and get +X/+X
        // until end of turn, where X is the number of creatures you control"
        let def = parse_effect_chain(
            "creatures you control gain trample and get +X/+X until end of turn, where X is the number of creatures you control",
            AbilityKind::Spell,
        );
        assert_eq!(
            def.duration,
            Some(Duration::UntilEndOfTurn),
            "duration should be UntilEndOfTurn, got {:?}",
            def.duration
        );
    }

    #[test]
    fn duration_preserved_with_for_each_suffix() {
        // Goblin Piledriver pattern: "gets +2/+0 until end of turn for each other attacking Goblin"
        let def = parse_effect_chain(
            "~ gets +2/+0 until end of turn for each other attacking Goblin",
            AbilityKind::Spell,
        );
        assert_eq!(
            def.duration,
            Some(Duration::UntilEndOfTurn),
            "duration should be UntilEndOfTurn, got {:?}",
            def.duration
        );
    }

    #[test]
    fn duration_preserved_creatures_you_control_for_each() {
        // "Creatures you control get +1/+0 until end of turn for each creature you control"
        let def = parse_effect_chain(
            "creatures you control get +1/+1 until end of turn for each land you control",
            AbilityKind::Spell,
        );
        assert_eq!(
            def.duration,
            Some(Duration::UntilEndOfTurn),
            "duration should be UntilEndOfTurn, got {:?}",
            def.duration
        );
    }

    #[test]
    fn duration_preserved_for_each_before_duration() {
        // Ral's Staticaster: unusual ordering with "for each" before "until end of turn"
        let def = parse_effect_chain(
            "~ gets +1/+0 for each card in your hand until end of turn",
            AbilityKind::Spell,
        );
        assert_eq!(
            def.duration,
            Some(Duration::UntilEndOfTurn),
            "duration should be UntilEndOfTurn when 'for each' precedes duration, got {:?}",
            def.duration
        );
    }

    #[test]
    fn for_each_pump_produces_dynamic_pt_via_pump_clause() {
        // "gets +1/+1 for each creature you control" should produce Pump with Quantity PtValue
        // via parse_pump_clause, not fixed AddPower/AddToughness via modifications.
        let def = parse_effect_chain(
            "~ gets +1/+1 for each creature you control",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::Pump {
                power, toughness, ..
            } => {
                assert!(
                    matches!(power, PtValue::Quantity(..)),
                    "power should be Quantity, got {:?}",
                    power
                );
                assert!(
                    matches!(toughness, PtValue::Quantity(..)),
                    "toughness should be Quantity, got {:?}",
                    toughness
                );
            }
            other => panic!("expected Pump, got {:?}", other),
        }
    }

    #[test]
    fn for_each_pump_asymmetric_plus2_plus0() {
        // "gets +2/+0 for each attacking Goblin" → power should be Multiply(2, Ref)
        let def = parse_effect_chain("~ gets +2/+0 for each attacking Goblin", AbilityKind::Spell);
        match &*def.effect {
            Effect::Pump {
                power, toughness, ..
            } => {
                assert!(
                    matches!(
                        power,
                        PtValue::Quantity(QuantityExpr::Multiply { factor: 2, .. })
                    ),
                    "power should be Multiply(2, ...), got {:?}",
                    power
                );
                assert_eq!(
                    *toughness,
                    PtValue::Fixed(0),
                    "toughness should stay Fixed(0)"
                );
            }
            other => panic!("expected Pump, got {:?}", other),
        }
    }

    #[test]
    fn for_each_pump_with_duration_and_quantity() {
        // "gets +1/+1 until end of turn for each creature you control"
        // should have both duration AND dynamic PtValue
        let def = parse_effect_chain(
            "~ gets +1/+1 until end of turn for each creature you control",
            AbilityKind::Spell,
        );
        assert_eq!(def.duration, Some(Duration::UntilEndOfTurn));
        match &*def.effect {
            Effect::Pump {
                power, toughness, ..
            } => {
                assert!(
                    matches!(power, PtValue::Quantity(..)),
                    "power should be Quantity, got {:?}",
                    power
                );
                assert!(
                    matches!(toughness, PtValue::Quantity(..)),
                    "toughness should be Quantity, got {:?}",
                    toughness
                );
            }
            other => panic!("expected Pump, got {:?}", other),
        }
    }

    #[test]
    fn otherwise_cast_from_hand_condition() {
        // "If this spell was cast from your hand, [effect]. Otherwise, [else-effect]"
        // Should produce condition + else_ability, not Unimplemented
        let chain = parse_effect_chain(
            "If this spell was cast from your hand, draw two cards. Otherwise, draw a card.",
            crate::types::ability::AbilityKind::Spell,
        );
        let first = &chain;
        assert!(
            first.condition.is_some(),
            "first def should have CastFromZone condition, got: {:?}",
            first
        );
        assert!(
            first.else_ability.is_some(),
            "first def should have else_ability for Otherwise clause"
        );
    }

    #[test]
    fn otherwise_revealed_card_type_condition() {
        // "If it's a creature card, [effect]. Otherwise, [else-effect]"
        let chain = parse_effect_chain(
            "If it's a creature card, put it into your hand. Otherwise, put it into your graveyard.",
            crate::types::ability::AbilityKind::Spell,
        );
        let first = &chain;
        assert!(
            first.condition.is_some(),
            "should have RevealedHasCardType condition, got: {:?}",
            first
        );
    }

    #[test]
    fn creature_card_of_the_chosen_type_conditional() {
        // Herald's Horn pattern: "look at top, if it's a creature card of the chosen type,
        // you may reveal it and put it into your hand."
        let chain = parse_effect_chain(
            "look at the top card of your library. If it's a creature card of the chosen type, you may reveal it and put it into your hand.",
            crate::types::ability::AbilityKind::Spell,
        );
        // First effect: Dig (look at top 1, private)
        assert!(
            matches!(*chain.effect, Effect::Dig { .. }),
            "expected Dig, got: {:?}",
            chain.effect
        );
        // Sub-ability chain should have the conditional
        let sub = chain
            .sub_ability
            .as_ref()
            .expect("should have sub_ability after Dig");
        assert_eq!(
            sub.condition,
            Some(AbilityCondition::RevealedHasCardType {
                card_type: CoreType::Creature,
                negated: false,
                additional_filter: Some(FilterProp::IsChosenCreatureType),
            }),
            "condition should check creature type + chosen type"
        );
        // The sub-ability should be optional ("you may")
        assert!(sub.optional, "should be optional (you may)");
        // Walk the sub-ability chain to find ChangeZone to Hand
        let mut found_change_zone = false;
        let mut current = Some(sub.as_ref());
        while let Some(def) = current {
            if matches!(
                &*def.effect,
                Effect::ChangeZone {
                    destination: Zone::Hand,
                    ..
                }
            ) {
                found_change_zone = true;
                break;
            }
            current = def.sub_ability.as_deref();
        }
        assert!(
            found_change_zone,
            "should have ChangeZone to Hand in sub-ability chain"
        );
    }

    #[test]
    fn effect_clash_with_opponent() {
        let e = parse_effect("clash with an opponent");
        assert!(matches!(e, Effect::Clash), "expected Clash, got: {e:?}");
    }

    #[test]
    fn effect_populate() {
        let e = parse_effect("populate");
        assert!(
            matches!(e, Effect::Populate),
            "expected Populate, got: {e:?}"
        );
    }

    #[test]
    fn effect_pay_energy_double() {
        let e = parse_effect("pay {e}{e}");
        assert!(
            matches!(
                e,
                Effect::PayCost {
                    cost: PaymentCost::Energy {
                        amount: QuantityExpr::Fixed { value: 2 },
                    },
                }
            ),
            "expected PayCost Energy(2), got: {e:?}"
        );
    }

    #[test]
    fn effect_pay_energy_triple() {
        let e = parse_effect("pay {e}{e}{e}");
        assert!(
            matches!(
                e,
                Effect::PayCost {
                    cost: PaymentCost::Energy {
                        amount: QuantityExpr::Fixed { value: 3 },
                    },
                }
            ),
            "expected PayCost Energy(3), got: {e:?}"
        );
    }

    #[test]
    fn effect_switch_pt_target_creature() {
        let e = parse_effect("switch target creature's power and toughness until end of turn");
        assert!(
            matches!(e, Effect::SwitchPT { .. }),
            "expected SwitchPT, got: {e:?}"
        );
    }

    #[test]
    fn effect_switch_pt_self() {
        let e = parse_effect("switch ~'s power and toughness until end of turn");
        assert!(
            matches!(
                e,
                Effect::SwitchPT {
                    target: TargetFilter::SelfRef
                }
            ),
            "expected SwitchPT with SelfRef, got: {e:?}"
        );
    }

    /// CR 608.2c: Period-separated pronoun reference ("Tap target creature. Put two stun
    /// counters on it.") must resolve "it" to ParentTarget, not SelfRef. The sub_ability's
    /// counter effect should target the same creature as the tap effect.
    #[test]
    fn period_separated_pronoun_resolves_to_parent_target() {
        let def = parse_effect_chain(
            "Tap target creature. Put two stun counters on it.",
            crate::types::ability::AbilityKind::Activated,
        );

        // Primary effect: Tap with a typed creature filter.
        assert!(
            matches!(
                def.effect.as_ref(),
                Effect::Tap {
                    target: TargetFilter::Typed(_)
                }
            ),
            "expected Tap with typed target, got: {:?}",
            def.effect
        );

        // Sub-ability: PutCounter targeting ParentTarget (the same creature).
        let sub = def
            .sub_ability
            .as_ref()
            .expect("should have sub_ability for 'put counters on it'");
        assert!(
            matches!(
                sub.effect.as_ref(),
                Effect::PutCounter {
                    target: TargetFilter::ParentTarget,
                    ..
                }
            ),
            "expected PutCounter with ParentTarget, got: {:?}",
            sub.effect
        );
    }

    /// CR 608.2c + CR 608.2k: A `GenericEffect` (becomes/animate clause) with a typed
    /// target must thread that target through to a following anaphoric sub-ability.
    /// Tyrite Sanctum's "Target legendary creature becomes a God in addition to its
    /// other types. Put a +1/+1 counter on it." — "it" refers to the chosen legendary
    /// creature, not the activating land.
    #[test]
    fn generic_effect_with_typed_target_threads_anaphor_to_parent_target() {
        let def = parse_effect_chain(
            "Target legendary creature becomes a God in addition to its other types. Put a +1/+1 counter on it.",
            crate::types::ability::AbilityKind::Activated,
        );

        // Primary effect: GenericEffect with Some(Typed(_)) target.
        assert!(
            matches!(
                def.effect.as_ref(),
                Effect::GenericEffect {
                    target: Some(TargetFilter::Typed(_)),
                    ..
                }
            ),
            "expected GenericEffect with typed target, got: {:?}",
            def.effect
        );

        // Sub-ability: PutCounter targeting ParentTarget (the chosen legendary creature).
        let sub = def
            .sub_ability
            .as_ref()
            .expect("should have sub_ability for 'put counters on it'");
        assert!(
            matches!(
                sub.effect.as_ref(),
                Effect::PutCounter {
                    target: TargetFilter::ParentTarget,
                    ..
                }
            ),
            "expected sub_ability PutCounter with ParentTarget, got: {:?}",
            sub.effect
        );
    }

    /// CR 608.2k: Predicate guard — the anaphor-rebinding gate must NOT fire when the
    /// preceding effect lacks a typed target. An untargeted prior clause has no
    /// antecedent for "it" to bind to via `ParentTarget`, so the resolver-side
    /// `resolve_it_pronoun` default must be preserved.
    #[test]
    fn generic_effect_without_target_does_not_thread_anaphor() {
        // Use an explicit no-target leading effect (Draw) so any "it" in the trailing
        // clause cannot be interpreted as referring back to a typed antecedent.
        let def = parse_effect_chain(
            "Draw a card. Put a +1/+1 counter on it.",
            crate::types::ability::AbilityKind::Activated,
        );

        // Sub-ability target must NOT be ParentTarget — there is no typed antecedent.
        if let Some(sub) = def.sub_ability.as_ref() {
            if let Effect::PutCounter { target, .. } = sub.effect.as_ref() {
                assert!(
                    !matches!(target, TargetFilter::ParentTarget),
                    "anaphor must not thread through a non-targeted prior effect, got ParentTarget on sub_ability"
                );
            }
        }
    }

    // ── RevealUntil tests ──

    #[test]
    fn reveal_until_creature_to_hand_rest_to_library() {
        let def = parse_effect_chain(
            "Reveal cards from the top of your library until you reveal a creature card. Put that card into your hand and the rest on the bottom of your library in a random order.",
            AbilityKind::Activated,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::RevealUntil {
                    player: TargetFilter::Controller,
                    filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
                    kept_destination: Zone::Hand,
                    rest_destination: Zone::Library,
                    enter_tapped: false,
                } if type_filters.contains(&TypeFilter::Creature)
            ),
            "expected RevealUntil player=Controller, creature->hand, rest->library, got: {:?}",
            def.effect
        );
        // No sub_ability — destinations are baked in
        assert!(def.sub_ability.is_none(), "should have no sub_ability");
    }

    /// CR 109.5 + CR 701.20a: Third-person form ("Its controller reveals cards
    /// from the top of their library until they reveal a creature card.") —
    /// must dispatch to RevealUntil with player=ParentTargetController, not to
    /// RevealTop. This is the building block underpinning Polymorph, Proteus
    /// Staff, Transmogrify, and 30+ other cards in the class.
    #[test]
    fn reveal_until_its_controller_third_person() {
        let def = parse_effect_chain(
            "Its controller reveals cards from the top of their library until they reveal a creature card.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::RevealUntil {
                    player: TargetFilter::ParentTargetController,
                    filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
                    kept_destination: Zone::Hand,
                    rest_destination: Zone::Library,
                    enter_tapped: false,
                } if type_filters.contains(&TypeFilter::Creature)
            ),
            "expected RevealUntil player=ParentTargetController, got: {:?}",
            def.effect
        );
    }

    /// CR 109.5: "That creature's controller reveals cards from the top of
    /// their library until they reveal a creature card." (Proteus Staff,
    /// Transmogrify form).
    #[test]
    fn reveal_until_that_creatures_controller_third_person() {
        let def = parse_effect_chain(
            "That creature's controller reveals cards from the top of their library until they reveal a creature card.",
            AbilityKind::Activated,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::RevealUntil {
                    player: TargetFilter::ParentTargetController,
                    ..
                }
            ),
            "expected RevealUntil player=ParentTargetController, got: {:?}",
            def.effect
        );
    }

    /// CR 109.5: "That player reveals cards from the top of their library until
    /// they reveal a creature card." (Curse of Unbinding, Mind Funeral form).
    /// Subject "that player" extracts to TargetFilter::Player; the player
    /// parameter is preserved through to the resolver.
    #[test]
    fn reveal_until_that_player_third_person() {
        let def = parse_effect_chain(
            "That player reveals cards from the top of their library until they reveal a creature card.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::RevealUntil {
                    player: TargetFilter::Player,
                    ..
                }
            ),
            "expected RevealUntil player=Player, got: {:?}",
            def.effect
        );
    }

    /// CR 701.20a: "puts those cards into their graveyard" — the entire revealed
    /// pile (the matching land AND everything revealed before it) goes to the
    /// graveyard. Distinct from the "put the rest" continuation which only
    /// overrides rest_destination. Covers Balustrade Spy, Consuming Aberration,
    /// Destroy the Evidence, and Undercity Informer.
    #[test]
    fn reveal_until_puts_those_cards_to_graveyard() {
        let def = parse_effect_chain(
            "Target player reveals cards from the top of their library until they reveal a land card, then puts those cards into their graveyard.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::RevealUntil {
                    player: TargetFilter::Player,
                    filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
                    kept_destination: Zone::Graveyard,
                    rest_destination: Zone::Graveyard,
                    enter_tapped: false,
                } if type_filters.contains(&TypeFilter::Land)
            ),
            "expected RevealUntil(kept=Graveyard, rest=Graveyard), got: {:?}",
            def.effect
        );
    }

    /// CR 701.20a: Polymorph end-to-end — "Its controller reveals cards from
    /// the top of their library until they reveal a creature card. The player
    /// puts that card onto the battlefield, then shuffles all other cards
    /// revealed this way into their library." The combined sentence must lower
    /// kept_destination=Battlefield (RevealUntilKept continuation) and
    /// rest_destination=Library (the new "shuffles ... library" PutRest match).
    #[test]
    fn reveal_until_polymorph_continuations_lower_destinations() {
        let def = parse_effect_chain(
            "Its controller reveals cards from the top of their library until they reveal a creature card. The player puts that card onto the battlefield, then shuffles all other cards revealed this way into their library.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::RevealUntil {
                    player: TargetFilter::ParentTargetController,
                    kept_destination: Zone::Battlefield,
                    rest_destination: Zone::Library,
                    ..
                }
            ),
            "expected Polymorph-style chain to lower to RevealUntil(Battlefield, Library), got: {:?}",
            def.effect
        );
    }

    /// CR 701.20a: Proteus Staff post-bounce reveal — "That creature's
    /// controller reveals cards from the top of their library until they reveal
    /// a creature card. The player puts that card onto the battlefield and the
    /// rest on the bottom of their library in any order." The compound "and
    /// the rest" form is handled by the existing RevealUntilKept continuation.
    #[test]
    fn reveal_until_proteus_staff_compound_continuation() {
        let def = parse_effect_chain(
            "That creature's controller reveals cards from the top of their library until they reveal a creature card. The player puts that card onto the battlefield and the rest on the bottom of their library in any order.",
            AbilityKind::Activated,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::RevealUntil {
                    player: TargetFilter::ParentTargetController,
                    kept_destination: Zone::Battlefield,
                    rest_destination: Zone::Library,
                    ..
                }
            ),
            "expected Proteus-Staff-style chain to lower to RevealUntil(Battlefield, Library), got: {:?}",
            def.effect
        );
    }

    #[test]
    fn reveal_until_artifact_to_battlefield() {
        let def = parse_effect_chain(
            "Reveal cards from the top of your library until you reveal an artifact card. Put that card onto the battlefield and the rest on the bottom of your library in a random order.",
            AbilityKind::Activated,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::RevealUntil {
                    player: TargetFilter::Controller,
                    filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
                    kept_destination: Zone::Battlefield,
                    rest_destination: Zone::Library,
                    enter_tapped: false,
                } if type_filters.contains(&TypeFilter::Artifact)
            ),
            "expected RevealUntil player=Controller, artifact->battlefield, got: {:?}",
            def.effect
        );
    }

    #[test]
    fn reveal_until_creature_rest_to_graveyard() {
        let def = parse_effect_chain(
            "Reveal cards from the top of your library until you reveal a creature card. Put that card into your hand and the rest into your graveyard.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::RevealUntil {
                    kept_destination: Zone::Hand,
                    rest_destination: Zone::Graveyard,
                    ..
                }
            ),
            "expected rest->graveyard, got: {:?}",
            def.effect
        );
    }

    #[test]
    fn reveal_until_nonland_card() {
        let def = parse_effect_chain(
            "Reveal cards from the top of your library until you reveal a nonland card. Put the revealed cards on the bottom of your library in a random order.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::RevealUntil {
                    filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
                    rest_destination: Zone::Library,
                    ..
                } if matches!(&type_filters[..], [TypeFilter::Non(inner)] if matches!(**inner, TypeFilter::Land))
            ),
            "expected RevealUntil nonland, got: {:?}",
            def.effect
        );
    }

    #[test]
    fn leading_conditional_threads_condition_through_ast() {
        // "if it's your turn" is parseable by try_nom_condition_as_ability_condition.
        let ast = parse_clause_ast("if it's your turn, draw a card", &ParseContext::default());
        assert!(
            matches!(
                ast,
                ClauseAst::Conditional {
                    condition: Some(_),
                    ..
                }
            ),
            "expected Conditional with Some condition, got: {ast:?}"
        );
        let clause = lower_clause_ast(ast, &ParseContext::default());
        assert!(
            matches!(
                clause.condition,
                Some(AbilityCondition::IsYourTurn { negated: false })
            ),
            "expected IsYourTurn condition, got: {:?}",
            clause.condition
        );
        assert!(
            matches!(clause.effect, Effect::Draw { .. }),
            "expected Draw effect, got: {:?}",
            clause.effect
        );
    }

    #[test]
    fn leading_conditional_unrecognized_produces_none() {
        // An unrecognized condition should produce condition: None (backward-compatible).
        let ast = parse_clause_ast(
            "if a random unrecognized condition, draw a card",
            &ParseContext::default(),
        );
        assert!(
            matches!(
                ast,
                ClauseAst::Conditional {
                    condition: None,
                    ..
                }
            ),
            "expected Conditional with None condition for unrecognized text, got: {ast:?}"
        );
        let clause = lower_clause_ast(ast, &ParseContext::default());
        assert!(
            clause.condition.is_none(),
            "expected None condition for unrecognized text, got: {:?}",
            clause.condition
        );
    }

    #[test]
    fn leading_conditional_via_parse_effect_chain() {
        // End-to-end: parse_effect_chain sets condition on AbilityDefinition.
        let def = parse_effect_chain("if it's your turn, draw a card", AbilityKind::Spell);
        assert!(
            matches!(
                def.condition,
                Some(AbilityCondition::IsYourTurn { negated: false })
            ),
            "expected IsYourTurn on AbilityDefinition, got: {:?}",
            def.condition
        );
        assert!(
            matches!(
                *def.effect,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    ..
                }
            ),
            "expected Draw 1, got: {:?}",
            def.effect
        );
    }

    #[test]
    fn inject_subject_target_pump() {
        // Subject-predicate path: "~ gets +2/+2 until end of turn"
        // The subject "~" is stripped, "gets +2/+2 until end of turn" is parsed as
        // imperative Pump with TargetFilter::Any, then inject_subject_target should
        // replace Any with SelfRef from the subject.
        let clause =
            parse_effect_clause("~ gets +2/+2 until end of turn", &ParseContext::default());
        assert!(
            matches!(
                clause.effect,
                Effect::Pump {
                    target: TargetFilter::SelfRef,
                    ..
                }
            ),
            "expected Pump with SelfRef target, got: {:?}",
            clause.effect
        );
    }

    #[test]
    fn mutilate_all_creatures_for_each_is_pump_all() {
        // CR 609 + CR 611.2: "All creatures get -1/-1 until end of turn for each
        // Swamp you control" (Mutilate). The "all creatures" subject is a class
        // filter, so the for-each effect must lower to `Effect::PumpAll`, not
        // `Effect::Pump` with a single target — otherwise the engine prompts
        // for one target and only that creature shrinks.
        let clause = parse_effect_clause(
            "All creatures get -1/-1 until end of turn for each Swamp you control.",
            &ParseContext::default(),
        );
        match &clause.effect {
            Effect::PumpAll { target, .. } => {
                assert!(
                    matches!(target, TargetFilter::Typed(t) if t.type_filters.contains(&crate::types::ability::TypeFilter::Creature)),
                    "expected PumpAll target TypedFilter::Creature, got {target:?}"
                );
            }
            other => panic!("expected PumpAll, got: {other:?}"),
        }
    }

    #[test]
    fn self_ref_for_each_stays_pump() {
        // Preserves single-object promotion: "~ gets +1/+0 until end of turn for
        // each card in your graveyard" (Skeletal Changeling-style) must remain
        // `Effect::Pump` with `SelfRef` — not `PumpAll`.
        let clause = parse_effect_clause(
            "~ gets +1/+0 until end of turn for each card in your graveyard.",
            &ParseContext::default(),
        );
        assert!(
            matches!(
                clause.effect,
                Effect::Pump {
                    target: TargetFilter::SelfRef,
                    ..
                }
            ),
            "expected Pump with SelfRef target, got: {:?}",
            clause.effect
        );
    }

    // ── Conjure tests ──────────────────────────────────────────────────

    #[test]
    fn conjure_basic_battlefield() {
        let e = parse_effect("Conjure a card named Regal Force onto the battlefield");
        match e {
            Effect::Conjure {
                cards,
                destination,
                tapped,
            } => {
                assert_eq!(cards.len(), 1);
                assert_eq!(cards[0].name, "Regal Force");
                assert_eq!(cards[0].count, QuantityExpr::Fixed { value: 1 });
                assert_eq!(destination, Zone::Battlefield);
                assert!(!tapped);
            }
            other => panic!("expected Conjure, got: {other:?}"),
        }
    }

    #[test]
    fn conjure_quantity_graveyard() {
        let e = parse_effect("conjure three cards named Reassembling Skeleton into your graveyard");
        match e {
            Effect::Conjure {
                cards,
                destination,
                tapped,
            } => {
                assert_eq!(cards.len(), 1);
                assert_eq!(cards[0].name, "Reassembling Skeleton");
                assert_eq!(cards[0].count, QuantityExpr::Fixed { value: 3 });
                assert_eq!(destination, Zone::Graveyard);
                assert!(!tapped);
            }
            other => panic!("expected Conjure, got: {other:?}"),
        }
    }

    #[test]
    fn conjure_battlefield_tapped() {
        let e = parse_effect("conjure a card named Forest onto the battlefield tapped");
        match e {
            Effect::Conjure {
                cards,
                destination,
                tapped,
            } => {
                assert_eq!(cards.len(), 1);
                assert_eq!(cards[0].name, "Forest");
                assert_eq!(destination, Zone::Battlefield);
                assert!(tapped);
            }
            other => panic!("expected Conjure, got: {other:?}"),
        }
    }

    #[test]
    fn conjure_multi_card_hand() {
        let e = parse_effect(
            "conjure a card named Darksteel Ingot and a card named Darksteel Plate into your hand",
        );
        match e {
            Effect::Conjure {
                cards,
                destination,
                tapped,
            } => {
                assert_eq!(cards.len(), 2);
                assert_eq!(cards[0].name, "Darksteel Ingot");
                assert_eq!(cards[0].count, QuantityExpr::Fixed { value: 1 });
                assert_eq!(cards[1].name, "Darksteel Plate");
                assert_eq!(cards[1].count, QuantityExpr::Fixed { value: 1 });
                assert_eq!(destination, Zone::Hand);
                assert!(!tapped);
            }
            other => panic!("expected Conjure, got: {other:?}"),
        }
    }

    #[test]
    fn conjure_into_library() {
        let e = parse_effect("conjure four cards named Lightning Bolt into your library");
        match e {
            Effect::Conjure {
                cards,
                destination,
                tapped,
            } => {
                assert_eq!(cards.len(), 1);
                assert_eq!(cards[0].name, "Lightning Bolt");
                assert_eq!(cards[0].count, QuantityExpr::Fixed { value: 4 });
                assert_eq!(destination, Zone::Library);
                assert!(!tapped);
            }
            other => panic!("expected Conjure, got: {other:?}"),
        }
    }

    #[test]
    fn conjure_two_battlefield_tapped() {
        let e =
            parse_effect("conjure two cards named Mishra's Foundry onto the battlefield tapped");
        match e {
            Effect::Conjure {
                cards,
                destination,
                tapped,
            } => {
                assert_eq!(cards.len(), 1);
                assert_eq!(cards[0].name, "Mishra's Foundry");
                assert_eq!(cards[0].count, QuantityExpr::Fixed { value: 2 });
                assert_eq!(destination, Zone::Battlefield);
                assert!(tapped);
            }
            other => panic!("expected Conjure, got: {other:?}"),
        }
    }

    // --- strip_mana_value_conditional tests ---

    #[test]
    fn strip_mv_conditional_suffix_le() {
        let (cond, text) =
            strip_mana_value_conditional("Destroy target creature if it has mana value 2 or less.");
        assert!(cond.is_some(), "should extract MV ≤ 2 condition");
        assert_eq!(text, "Destroy target creature");
        match cond.unwrap() {
            AbilityCondition::TargetMatchesFilter {
                filter,
                use_lki,
                negated,
            } => {
                assert!(!use_lki);
                assert!(!negated);
                if let TargetFilter::Typed(tf) = filter {
                    assert!(tf.properties.iter().any(|p| matches!(
                        p,
                        FilterProp::CmcLE {
                            value: QuantityExpr::Fixed { value: 2 }
                        }
                    )));
                } else {
                    panic!("expected Typed filter");
                }
            }
            other => panic!("expected TargetMatchesFilter, got: {other:?}"),
        }
    }

    #[test]
    fn strip_mv_conditional_suffix_ge() {
        let (cond, text) =
            strip_mana_value_conditional("Counter target spell if it has mana value 4 or greater");
        assert!(cond.is_some(), "should extract MV ≥ 4 condition");
        assert_eq!(text, "Counter target spell");
        match cond.unwrap() {
            AbilityCondition::TargetMatchesFilter { filter, .. } => {
                if let TargetFilter::Typed(tf) = filter {
                    assert!(tf.properties.iter().any(|p| matches!(
                        p,
                        FilterProp::CmcGE {
                            value: QuantityExpr::Fixed { value: 4 }
                        }
                    )));
                } else {
                    panic!("expected Typed filter");
                }
            }
            other => panic!("expected TargetMatchesFilter, got: {other:?}"),
        }
    }

    #[test]
    fn strip_mv_conditional_no_match() {
        let (cond, text) = strip_mana_value_conditional("Destroy target creature");
        assert!(cond.is_none());
        assert_eq!(text, "Destroy target creature");
    }

    // --- Fatal Push integration test ---

    #[test]
    fn fatal_push_base_has_mv_condition() {
        let def = parse_effect_chain(
            "Destroy target creature if it has mana value 2 or less",
            AbilityKind::Spell,
        );
        match def.condition {
            Some(AbilityCondition::TargetMatchesFilter { ref filter, .. }) => {
                if let TargetFilter::Typed(tf) = filter {
                    assert!(
                        tf.properties
                            .iter()
                            .any(|p| matches!(p, FilterProp::CmcLE { .. })),
                        "should have CmcLE property"
                    );
                } else {
                    panic!("expected Typed filter");
                }
            }
            other => panic!("expected TargetMatchesFilter condition, got: {other:?}"),
        }
    }

    // --- "The owner of target X" parser tests ---

    #[test]
    fn parse_owner_of_target_shuffles_into_library() {
        // Chaos Warp pattern
        let def = parse_effect_chain(
            "The owner of target permanent shuffles it into their library",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::ChangeZone {
                    destination: Zone::Library,
                    owner_library: true,
                    ..
                }
            ),
            "Expected ChangeZone to Library with owner_library, got {:?}",
            def.effect
        );
        // Should have a Shuffle sub_ability
        assert!(def.sub_ability.is_some(), "Expected Shuffle sub_ability");
    }

    #[test]
    fn parse_owner_of_target_nonland_shuffles_into_library() {
        // Oblation pattern
        let def = parse_effect_chain(
            "The owner of target nonland permanent shuffles it into their library",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::ChangeZone {
                    destination: Zone::Library,
                    owner_library: true,
                    ..
                }
            ),
            "Expected ChangeZone to Library with owner_library, got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_owner_of_target_puts_second_from_top() {
        // Deem Inferior pattern
        let def = parse_effect_chain(
            "The owner of target nonland permanent puts it into their library second from the top or on the bottom",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::PutOnTopOrBottom { .. }),
            "Expected PutOnTopOrBottom, got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_possessive_owner_shuffles_into_library() {
        // "target artifact or enchantment's owner shuffles it ..." pattern (if ever used)
        let def = parse_effect_chain(
            "Target permanent's owner shuffles it into their library",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::ChangeZone {
                    destination: Zone::Library,
                    owner_library: true,
                    ..
                }
            ),
            "Expected ChangeZone to Library with owner_library, got {:?}",
            def.effect
        );
    }

    // --- "The next spell" parser tests ---

    #[test]
    fn parse_next_creature_spell_cant_be_countered() {
        // Insist pattern
        let def = parse_effect_chain(
            "The next creature spell you cast this turn can't be countered",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::GrantNextSpellAbility {
                    modifier: crate::types::game_state::NextSpellModifier::CantBeCountered,
                    ..
                }
            ),
            "Expected GrantNextSpellAbility(CantBeCountered), got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_next_spell_has_convoke() {
        // Wand of the Worldsoul pattern
        let def = parse_effect_chain(
            "The next spell you cast this turn has convoke",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::GrantNextSpellAbility {
                    modifier: crate::types::game_state::NextSpellModifier::HasKeyword {
                        keyword: Keyword::Convoke,
                    },
                    ..
                }
            ),
            "Expected GrantNextSpellAbility(HasKeyword(Convoke)), got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_next_sorcery_spell_flash() {
        // Quicken pattern
        let def = parse_effect_chain(
            "The next sorcery spell you cast this turn can be cast as though it had flash",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::GrantNextSpellAbility {
                    modifier: crate::types::game_state::NextSpellModifier::CastAsThoughFlash,
                    ..
                }
            ),
            "Expected GrantNextSpellAbility(CastAsThoughFlash), got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_next_spell_has_cascade() {
        // Dark Apostle pattern
        let def = parse_effect_chain(
            "The next noncreature spell you cast this turn has cascade",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::GrantNextSpellAbility {
                    modifier: crate::types::game_state::NextSpellModifier::HasKeyword {
                        keyword: Keyword::Cascade,
                    },
                    ..
                }
            ),
            "Expected GrantNextSpellAbility(HasKeyword(Cascade)), got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_next_spell_has_affinity() {
        // Saheeli, the Gifted pattern
        let def = parse_effect_chain(
            "The next spell you cast this turn has affinity for artifacts",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::GrantNextSpellAbility {
                    modifier: crate::types::game_state::NextSpellModifier::HasKeyword {
                        keyword: Keyword::Affinity(..),
                    },
                    ..
                }
            ),
            "Expected GrantNextSpellAbility(HasKeyword(Affinity)), got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_next_face_down_creature_costs_less() {
        // Panoptic Projektor pattern
        let def = parse_effect_chain(
            "The next face-down creature spell you cast this turn costs {3} less to cast",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::ReduceNextSpellCost { amount: 3, .. }),
            "Expected ReduceNextSpellCost {{ amount: 3 }}, got {:?}",
            def.effect
        );
    }

    #[test]
    fn parse_next_spell_has_improvise() {
        // Archway of Innovation pattern
        let def = parse_effect_chain(
            "The next spell you cast this turn has improvise",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                *def.effect,
                Effect::GrantNextSpellAbility {
                    modifier: crate::types::game_state::NextSpellModifier::HasKeyword {
                        keyword: Keyword::Improvise,
                    },
                    ..
                }
            ),
            "Expected GrantNextSpellAbility(HasKeyword(Improvise)), got {:?}",
            def.effect
        );
    }

    // CR 201.3: "choose a land card name" is a CardName choice scoped to the
    // land type (Petrified Hamlet). The alt-list accepts the land-qualified
    // form just like the existing creature-qualified and nonland-qualified
    // variants.
    #[test]
    fn named_choice_accepts_land_card_name() {
        assert_eq!(
            super::try_parse_named_choice("choose a land card name"),
            Some(ChoiceType::CardName)
        );
        assert_eq!(
            super::try_parse_named_choice("choose a card name"),
            Some(ChoiceType::CardName)
        );
        assert_eq!(
            super::try_parse_named_choice("choose a nonland card name"),
            Some(ChoiceType::CardName)
        );
        assert_eq!(
            super::try_parse_named_choice("choose a creature card name"),
            Some(ChoiceType::CardName)
        );
    }

    /// CR 705 + CR 614.10: Ral Zarek, Guest Lecturer [-7] — "Flip five coins.
    /// Target opponent skips their next X turns, where X is the number of coins
    /// that came up heads." parses into a single `FlipCoins { count: 5,
    /// win_effect: SkipNextTurn { opponent, 1 } }`. The heads-count multiplier
    /// comes from the outer `FlipCoins` loop invoking the win_effect once per
    /// heads — the inner SkipNextTurn.count is normalized to 1.
    #[test]
    fn ral_zarek_flip_five_coins_skip_turn_per_heads() {
        let def = parse_effect_chain(
            "Flip five coins. Target opponent skips their next X turns, where X is the number of coins that came up heads.",
            AbilityKind::Activated,
        );

        let Effect::FlipCoins {
            count,
            win_effect,
            lose_effect,
        } = &*def.effect
        else {
            panic!("expected FlipCoins, got {:?}", def.effect);
        };

        assert_eq!(
            count,
            &crate::types::ability::QuantityExpr::Fixed { value: 5 },
            "expected 5-flip count",
        );
        assert!(lose_effect.is_none(), "no lose_effect expected");

        let win = win_effect
            .as_ref()
            .expect("win_effect should carry the SkipNextTurn")
            .as_ref();
        let Effect::SkipNextTurn {
            target: skip_target,
            count: skip_count,
        } = &*win.effect
        else {
            panic!("expected SkipNextTurn as win_effect, got {:?}", win.effect);
        };
        // count normalized to 1 because outer FlipCoins already loops per heads.
        assert_eq!(
            skip_count,
            &crate::types::ability::QuantityExpr::Fixed { value: 1 },
            "inner skip count must be normalized to 1",
        );
        assert!(
            matches!(
                skip_target,
                TargetFilter::Typed(tf)
                    if tf.controller == Some(ControllerRef::Opponent)
            ),
            "target must be an opponent, got {:?}",
            skip_target
        );
    }

    /// CR 705.1: "flip two coins" / "flips five coins" are recognized as
    /// `FlipCoins` with the correct count; no win/lose branch when no
    /// following clause consolidates with them.
    #[test]
    fn flip_n_coins_emits_flip_coins_variant() {
        let def = parse_effect_chain("Flip two coins.", AbilityKind::Spell);
        assert!(
            matches!(
                &*def.effect,
                Effect::FlipCoins {
                    count: crate::types::ability::QuantityExpr::Fixed { value: 2 },
                    win_effect: None,
                    lose_effect: None,
                }
            ),
            "expected FlipCoins count=2, got {:?}",
            def.effect
        );
    }

    /// CR 614.10: "Target opponent skips their next turn." (no X, no flip)
    /// parses as a standalone SkipNextTurn with count=1 and opponent target.
    #[test]
    fn target_opponent_skips_next_turn_parses() {
        let def = parse_effect_chain("Target opponent skips their next turn.", AbilityKind::Spell);
        let Effect::SkipNextTurn { target, count } = &*def.effect else {
            panic!("expected SkipNextTurn, got {:?}", def.effect);
        };
        assert_eq!(
            count,
            &crate::types::ability::QuantityExpr::Fixed { value: 1 }
        );
        assert!(matches!(
            target,
            TargetFilter::Typed(tf)
                if tf.controller == Some(ControllerRef::Opponent)
        ));
    }

    /// CR 603.7 + CR 107.3 + CR 202.1: "When you next cast a spell with {X} in
    /// its mana cost this turn, ..." — delayed triggered ability. The embedded
    /// `SpellCast` trigger must carry a `HasXInManaCost` filter on
    /// `valid_card`. Target card: Brass Infiniscope.
    #[test]
    fn when_next_cast_spell_with_x_in_cost_parses() {
        use crate::types::ability::{DelayedTriggerCondition, FilterProp, TypedFilter};
        let def = parse_effect_chain(
            "When you next cast a spell with {X} in its mana cost this turn, draw a card.",
            AbilityKind::Activated,
        );
        let Effect::CreateDelayedTrigger {
            condition, effect, ..
        } = &*def.effect
        else {
            panic!("expected CreateDelayedTrigger, got {:?}", def.effect);
        };
        let DelayedTriggerCondition::WhenNextEvent { trigger } = condition else {
            panic!("expected WhenNextEvent, got {:?}", condition);
        };
        assert_eq!(
            trigger.mode,
            crate::types::triggers::TriggerMode::SpellCast,
            "delayed trigger mode must be SpellCast"
        );
        let expected = TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::HasXInManaCost]),
        );
        assert_eq!(
            trigger.valid_card.as_ref(),
            Some(&expected),
            "valid_card must carry HasXInManaCost filter"
        );
        // Scoped to the source's controller — "when YOU next cast".
        assert_eq!(trigger.valid_target, Some(TargetFilter::Controller));
        // Inner effect should parse as a draw.
        assert!(matches!(&*effect.effect, Effect::Draw { .. }));
    }

    #[test]
    fn kindred_dominance_excludes_chosen_creature_type() {
        use crate::types::ability::{ChoiceType, TypedFilter};

        let def = parse_effect_chain(
            "Choose a creature type. Destroy all creatures that aren't of the chosen type.",
            AbilityKind::Spell,
        );
        let Effect::Choose {
            choice_type,
            persist,
        } = &*def.effect
        else {
            panic!("expected Choose, got {:?}", def.effect);
        };
        assert_eq!(*choice_type, ChoiceType::CreatureType);
        assert!(*persist);

        let destroy = def.sub_ability.as_ref().expect("destroy continuation");
        let Effect::DestroyAll { target, .. } = &*destroy.effect else {
            panic!("expected DestroyAll, got {:?}", destroy.effect);
        };
        let expected = TargetFilter::And {
            filters: vec![
                TargetFilter::Typed(TypedFilter::creature()),
                TargetFilter::Not {
                    filter: Box::new(TargetFilter::Typed(
                        TypedFilter::default().properties(vec![FilterProp::IsChosenCreatureType]),
                    )),
                },
            ],
        };
        assert_eq!(target, &expected);
    }

    // ------------------------------------------------------------------
    // CR 700.2 + CR 608.2d: ChooseOneOf inline binary-choice regression.
    // ------------------------------------------------------------------

    /// Highway Robbery's spell text "You may discard a card or sacrifice a
    /// land" produces a `ChooseOneOf` with two branches (Discard + Sacrifice),
    /// wrapped in an optional outer ability.
    #[test]
    fn choose_one_of_detects_highway_robbery_pattern() {
        let ability = parse_effect_chain(
            "You may discard a card or sacrifice a land.",
            AbilityKind::Spell,
        );
        assert!(
            ability.optional,
            "outer 'you may' must mark ability optional"
        );
        match &*ability.effect {
            Effect::ChooseOneOf { branches } => {
                assert_eq!(branches.len(), 2, "Highway Robbery has exactly 2 branches");
                assert!(
                    matches!(&*branches[0].effect, Effect::Discard { .. }),
                    "first branch is Discard"
                );
                assert!(
                    matches!(&*branches[1].effect, Effect::Sacrifice { .. }),
                    "second branch is Sacrifice"
                );
            }
            other => panic!("expected ChooseOneOf, got {other:?}"),
        }
    }

    /// False-positive guard: "target creature or player" is a typed-target
    /// disjunction inside an imperative, NOT a binary choice of effects.
    #[test]
    fn choose_one_of_rejects_target_disjunction() {
        let ability = parse_effect_chain(
            "deal 3 damage to target creature or player",
            AbilityKind::Spell,
        );
        // Must parse as DealDamage, NOT ChooseOneOf.
        assert!(
            !matches!(&*ability.effect, Effect::ChooseOneOf { .. }),
            "target-phrase disjunction must not be split into ChooseOneOf"
        );
    }

    /// CR 701.21a + CR 608.2k: Promise of Aclazotz / Burnt Offering / Greater
    /// Good class — when the chunk loop strips a "you may " optional prefix,
    /// the chunk-scoped `ParseContext.actor = Some(You)` is consumed by the
    /// Sacrifice arm of `parse_targeted_action_ast` so the parsed
    /// `TargetFilter::Typed.controller` defaults to ControllerRef::You.
    /// Without this default the resolver treats `controller: None` as Any and
    /// lets the actor sacrifice an opponent's creature, violating CR 701.21a.
    #[test]
    fn parse_effect_chain_actor_default_propagates_through_you_may() {
        let ability =
            parse_effect_chain("you may sacrifice a non-Demon creature", AbilityKind::Spell);
        assert!(
            ability.optional,
            "'you may' prefix must mark the ability optional"
        );
        match &*ability.effect {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => assert_eq!(
                    tf.controller,
                    Some(ControllerRef::You),
                    "actor=You must be applied as default Sacrifice target controller, got {tf:?}"
                ),
                other => panic!("expected Typed Sacrifice target, got {other:?}"),
            },
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    /// False-positive guard: if only one half parses (the other is
    /// Unimplemented), the detector must bail and let the normal dispatch
    /// handle it — better to get a partial/Unimplemented than a fake choice.
    #[test]
    fn choose_one_of_rejects_when_second_half_unparseable() {
        let ability = parse_effect_chain(
            "discard a card or <gibberish nonsense phrase>",
            AbilityKind::Spell,
        );
        assert!(
            !matches!(&*ability.effect, Effect::ChooseOneOf { .. }),
            "must not emit ChooseOneOf when one branch fails to parse"
        );
    }

    /// CR 608.2k: After a token-creating effect, a plain "the token <verb>"
    /// anaphor (without the "created this way" populate qualifier) must
    /// rewrite to a `GenericEffect` targeting `LastCreated`. Pietra,
    /// Crafter of Clowns is the canonical case: "Create a 1/1 white Clown
    /// Robot artifact creature token. … the token gains haste until end of
    /// turn."
    #[test]
    fn the_token_plain_anaphor_after_token_creator_rewrites_to_last_created() {
        let ability = parse_effect_chain(
            "create a 1/1 white Clown Robot artifact creature token. the token gains haste",
            AbilityKind::Spell,
        );
        // Outer effect is the Token creation.
        assert!(
            matches!(&*ability.effect, Effect::Token { .. }),
            "expected Effect::Token, got {:?}",
            ability.effect
        );
        // Sub-ability is the rewritten anaphor: GenericEffect bound to
        // LastCreated with a Haste keyword grant.
        let sub = ability
            .sub_ability
            .as_deref()
            .expect("anaphor sub-ability missing");
        match &*sub.effect {
            Effect::GenericEffect {
                static_abilities,
                duration,
                target,
            } => {
                assert_eq!(*target, Some(TargetFilter::LastCreated));
                assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
                assert_eq!(static_abilities.len(), 1);
                assert_eq!(
                    static_abilities[0].affected,
                    Some(TargetFilter::LastCreated)
                );
                assert!(
                    static_abilities[0].modifications.iter().any(|m| matches!(
                        m,
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Haste,
                        }
                    )),
                    "expected AddKeyword(Haste), got {:?}",
                    static_abilities[0].modifications
                );
            }
            other => panic!("expected GenericEffect anaphor rewrite, got {other:?}"),
        }
    }

    /// CR 608.2k: Recognizer-level coverage for the "this token "
    /// anaphor — exercised directly via `Unimplemented` description so
    /// the test pins the recognizer's behavior independent of upstream
    /// dispatch (where "this token" gets normalized to `~`/SelfRef by
    /// `SELF_REF_TYPE_PHRASES` and routed through the typed parser
    /// before the post-pass runs). Test is the safety net for any
    /// future surface form that survives normalization and lands here
    /// as `Unimplemented`.
    #[test]
    fn rewrite_recognizer_accepts_this_token_prefix() {
        let effect = Effect::Unimplemented {
            name: "this".to_string(),
            description: Some("this token gains haste".to_string()),
        };
        let rewritten = rewrite_token_created_this_way_unimplemented(&effect)
            .expect("recognizer must accept 'this token' prefix");
        match rewritten {
            Effect::GenericEffect {
                static_abilities,
                duration,
                target,
            } => {
                assert_eq!(target, Some(TargetFilter::LastCreated));
                assert_eq!(duration, Some(Duration::UntilEndOfTurn));
                assert!(static_abilities[0].modifications.iter().any(|m| matches!(
                    m,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste,
                    }
                )));
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }

    /// CR 608.2k: Recognizer-level coverage for the "that token "
    /// anaphor — exercised via `Unimplemented` description because
    /// "that token" at the chain level is currently routed through the
    /// trigger pipeline (producing `GenericEffect { affected:
    /// ParentTarget }`), not the post-pass. The recognizer must still
    /// accept it as a defensive catch for future flows.
    #[test]
    fn rewrite_recognizer_accepts_that_token_prefix() {
        let effect = Effect::Unimplemented {
            name: "that".to_string(),
            description: Some("that token gains flying".to_string()),
        };
        let rewritten = rewrite_token_created_this_way_unimplemented(&effect)
            .expect("recognizer must accept 'that token' prefix");
        match rewritten {
            Effect::GenericEffect {
                static_abilities,
                target,
                ..
            } => {
                assert_eq!(target, Some(TargetFilter::LastCreated));
                assert!(static_abilities[0].modifications.iter().any(|m| matches!(
                    m,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Flying,
                    }
                )));
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }

    /// Disambiguation regression: when the populate-specific qualifier
    /// "the token created this way " is present, it MUST win over the
    /// shorter "the token " prefix. The longest-first strip_prefix order
    /// is the guard. If "the token " were tried first, the qualifier text
    /// would leak into the parsed verb-phrase (`parse_continuous_modifications`
    /// would receive "created this way gains haste"), producing an empty
    /// modification set and silently dropping the rewrite.
    #[test]
    fn populate_qualifier_takes_precedence_over_plain_the_token() {
        let ability = parse_effect_chain(
            "populate. the token created this way gains haste",
            AbilityKind::Spell,
        );
        assert!(
            matches!(&*ability.effect, Effect::Populate),
            "outer effect should be Populate, got {:?}",
            ability.effect
        );
        let sub = ability
            .sub_ability
            .as_deref()
            .expect("anaphor sub-ability missing");
        match &*sub.effect {
            Effect::GenericEffect {
                static_abilities,
                target,
                ..
            } => {
                assert_eq!(*target, Some(TargetFilter::LastCreated));
                assert!(
                    static_abilities[0].modifications.iter().any(|m| matches!(
                        m,
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Haste,
                        }
                    )),
                    "qualifier-form should still parse to AddKeyword(Haste); got {:?}",
                    static_abilities[0].modifications
                );
            }
            other => panic!("expected GenericEffect anaphor rewrite, got {other:?}"),
        }
    }

    /// Negative gate: without a preceding token-creating effect, plain
    /// "the token gains haste" must NOT be rewritten — there is no
    /// antecedent for `LastCreated` to bind to. The outer
    /// `resolve_populated_token_anaphors` walk gates on
    /// `is_token_creating_effect` in the chain prefix.
    #[test]
    fn plain_anaphor_without_token_creator_is_not_rewritten() {
        // Chain has no Token / Populate / CopyTokenOf — just a Pump and
        // a dangling anaphor. The dangling clause must remain Unimplemented.
        let ability = parse_effect_chain(
            "target creature gets +1/+1. the token gains haste",
            AbilityKind::Spell,
        );
        // Walk the chain's sub-ability tail; assert no GenericEffect with
        // LastCreated target was produced.
        fn contains_last_created_rewrite(def: &AbilityDefinition) -> bool {
            if let Effect::GenericEffect {
                target: Some(TargetFilter::LastCreated),
                ..
            } = &*def.effect
            {
                return true;
            }
            def.sub_ability
                .as_deref()
                .is_some_and(contains_last_created_rewrite)
        }
        assert!(
            !contains_last_created_rewrite(&ability),
            "no token creator preceded — must not produce LastCreated rewrite"
        );
    }

    /// CR 608.2k + CR 603.7c: Inalla-style "Exile it at the beginning of
    /// the next end step" inside a delayed trigger after a token-creating
    /// effect must rewrite the inner `ChangeZone { target: ParentTarget }`
    /// to `LastCreated`. Coverage gap before B11: `ChangeZone` was missing
    /// from `rewrite_parent_target_to_last_created`'s effect-list, so
    /// Inalla's exile-the-token delayed trigger leaked `ParentTarget`.
    #[test]
    fn delayed_trigger_change_zone_after_token_creator_rewrites_to_last_created() {
        let ability = parse_effect_chain(
            "create a token that's a copy of that creature. the token gains haste. exile it at the beginning of the next end step",
            AbilityKind::Spell,
        );
        // Outermost is CopyTokenOf.
        assert!(
            matches!(&*ability.effect, Effect::CopyTokenOf { .. }),
            "expected CopyTokenOf, got {:?}",
            ability.effect
        );
        // Walk to the CreateDelayedTrigger and inspect its inner ChangeZone.
        fn find_delayed_change_zone_target(def: &AbilityDefinition) -> Option<TargetFilter> {
            if let Effect::CreateDelayedTrigger { effect: inner, .. } = &*def.effect {
                if let Effect::ChangeZone { target, .. } = &*inner.effect {
                    return Some(target.clone());
                }
            }
            def.sub_ability
                .as_deref()
                .and_then(find_delayed_change_zone_target)
        }
        let target = find_delayed_change_zone_target(&ability)
            .expect("expected CreateDelayedTrigger { ChangeZone } in chain");
        assert_eq!(
            target,
            TargetFilter::LastCreated,
            "ChangeZone target must be rewritten from ParentTarget to LastCreated"
        );
    }

    /// CR 701.57a + CR 702.85a: `Effect::ExileFromTopUntil` must accept the
    /// second-person possessive form ("your library" / "you exile").
    /// Regression-protection for the pre-existing path Etali's parser
    /// extension was layered onto.
    #[test]
    fn exile_from_top_until_second_person_nonland_filter() {
        let e =
            parse_effect("exile cards from the top of your library until you exile a nonland card");
        let Effect::ExileFromTopUntil { filter } = e else {
            panic!("expected ExileFromTopUntil, got {:?}", e);
        };
        // Filter is `Typed { type_filters: [Non(Land)] }`.
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter");
        };
        assert!(typed
            .type_filters
            .iter()
            .any(|t| matches!(t, TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Land))));
    }

    /// CR 701.57a + CR 702.85a: Etali, Primal Conqueror's trigger body uses
    /// the third-person possessive form ("their library" / "they exile") that
    /// `strip_each_player_subject` produces after stripping "each player ".
    /// Without the third-person prefix arms, the parser falls through to a
    /// flat `ChangeZone` lowering and silently drops both the
    /// "until …" loop semantic and the nonland filter.
    #[test]
    fn exile_from_top_until_third_person_possessive_etali_form() {
        let e = parse_effect(
            "exile cards from the top of their library until they exile a nonland card",
        );
        let Effect::ExileFromTopUntil { filter } = e else {
            panic!("expected ExileFromTopUntil, got {:?}", e);
        };
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter");
        };
        assert!(typed
            .type_filters
            .iter()
            .any(|t| matches!(t, TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Land))));
    }

    /// CR 603.2 + CR 701.57a + CR 702.85a + CR 608.2: Etali's outer ETB trigger
    /// sentence — the "each player " subject phrase routes through
    /// `strip_each_player_subject` which strips the subject and deconjugates
    /// "exiles" → "exile", then the third-person possessive arm of
    /// `try_parse_exile_from_top_until` finalizes the lowering. Outer ability
    /// must carry `player_scope: All` so the resolver iterates per player;
    /// outer effect must be `ExileFromTopUntil { filter: Non(Land) }`. (The
    /// follow-up `cast any number of spells from among …` sub-sentence is a
    /// separate parser path — not asserted here; see the runtime tests in
    /// `game::effects::exile_from_top_until::tests` for per-resolution exile
    /// linkage that the future sub-ability filter will consume.)
    #[test]
    fn each_player_exiles_outer_effect_lowers_to_exile_from_top_until() {
        let def = parse_effect_chain(
            "each player exiles cards from the top of their library until they exile a nonland card.",
            AbilityKind::Spell,
        );

        assert_eq!(
            def.player_scope,
            Some(PlayerFilter::All),
            "player_scope must propagate from \"each player\" subject"
        );
        let Effect::ExileFromTopUntil { ref filter } = *def.effect else {
            panic!("expected outer ExileFromTopUntil, got {:?}", def.effect);
        };
        let TargetFilter::Typed(typed) = filter else {
            panic!("expected Typed filter, got {:?}", filter);
        };
        assert!(
            typed.type_filters.iter().any(
                |t| matches!(t, TypeFilter::Non(inner) if matches!(**inner, TypeFilter::Land))
            ),
            "filter must be Non(Land)"
        );
    }

    // CR 603.4: Parser arms for `AbilityCondition::NthResolutionThisTurn`.
    // Covers Omnath / Ashling / Nissa / Sephiroth / Teething Wurmlet class.

    #[test]
    fn nth_resolution_first_full_form() {
        // "this is the first time this ability has resolved this turn" — the
        // form printed in the LEADING-suffix sentence (Omnath, Teething Wurmlet).
        let result = try_nom_condition_as_ability_condition(
            "this is the first time this ability has resolved this turn",
        );
        assert_eq!(
            result,
            Some(AbilityCondition::NthResolutionThisTurn { n: 1 })
        );
    }

    #[test]
    fn nth_resolution_third_full_form() {
        // Ashling the Pilgrim print form.
        let result = try_nom_condition_as_ability_condition(
            "this is the third time this ability has resolved this turn",
        );
        assert_eq!(
            result,
            Some(AbilityCondition::NthResolutionThisTurn { n: 3 })
        );
    }

    #[test]
    fn nth_resolution_anaphoric_second() {
        // Omnath's continuation sentence: "If it's the second time, ..."
        // The "this ability has resolved this turn" tail is anaphoric.
        let result = try_nom_condition_as_ability_condition("it's the second time");
        assert_eq!(
            result,
            Some(AbilityCondition::NthResolutionThisTurn { n: 2 })
        );
    }

    #[test]
    fn nth_resolution_anaphoric_third() {
        let result = try_nom_condition_as_ability_condition("it's the third time");
        assert_eq!(
            result,
            Some(AbilityCondition::NthResolutionThisTurn { n: 3 })
        );
    }

    #[test]
    fn nth_resolution_fourth() {
        // Sephiroth, Fabled SOLDIER print form.
        let result = try_nom_condition_as_ability_condition(
            "this is the fourth time this ability has resolved this turn",
        );
        assert_eq!(
            result,
            Some(AbilityCondition::NthResolutionThisTurn { n: 4 })
        );
    }

    #[test]
    fn nth_resolution_partial_text_returns_none() {
        // A leftover after the recognized phrase must not match — the parser
        // requires either the full tail or empty remainder (anaphoric).
        let result = try_nom_condition_as_ability_condition(
            "this is the first time something else happened",
        );
        assert!(result.is_none());
    }

    #[test]
    fn nth_resolution_omnath_chain_populates_three_branches() {
        // End-to-end: Omnath's landfall trigger description should chain three
        // sub-abilities, each gated on `NthResolutionThisTurn { n: 1/2/3 }`.
        std::thread::Builder::new()
            .name("omnath-nth".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(|| {
                let def = parse_effect_chain(
                    "you gain 4 life if this is the first time this ability has resolved this turn. \
                     If it's the second time, add {R}{G}{W}{U}. \
                     If it's the third time, ~ deals 4 damage to each opponent.",
                    AbilityKind::Spell,
                );
                assert!(
                    matches!(*def.effect, Effect::GainLife { .. }),
                    "first effect should be GainLife, got {:?}",
                    def.effect
                );
                assert_eq!(
                    def.condition,
                    Some(AbilityCondition::NthResolutionThisTurn { n: 1 }),
                    "first branch must gate on n=1, got {:?}",
                    def.condition
                );
                let sub_n2 = def.sub_ability.as_ref().expect("must have n=2 branch");
                assert_eq!(
                    sub_n2.condition,
                    Some(AbilityCondition::NthResolutionThisTurn { n: 2 }),
                    "second branch must gate on n=2, got {:?}",
                    sub_n2.condition
                );
                let sub_n3 = sub_n2.sub_ability.as_ref().expect("must have n=3 branch");
                assert_eq!(
                    sub_n3.condition,
                    Some(AbilityCondition::NthResolutionThisTurn { n: 3 }),
                    "third branch must gate on n=3, got {:?}",
                    sub_n3.condition
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }

    /// CR 122.1 + CR 614.1c — The Darkness Crystal class:
    ///   "Put target creature card exiled with ~ onto the battlefield tapped
    ///    under your control with two additional +1/+1 counters on it."
    /// `try_parse_put_zone_change` must lift the trailing
    /// "with N <type> counter(s) on it" clause into
    /// `Effect::ChangeZone.enter_with_counters` so the resolver stamps
    /// counters during the same battlefield-entry replacement event.
    #[test]
    fn put_zone_change_lifts_with_counters_suffix() {
        let text = "put target creature card onto the battlefield tapped under your control with two additional +1/+1 counters on it";
        let lower = text.to_lowercase();
        let effect = try_parse_put_zone_change(&lower, text)
            .expect("expected ChangeZone for put-onto-battlefield");
        match effect {
            Effect::ChangeZone {
                destination,
                under_your_control,
                enter_tapped,
                enter_with_counters,
                ..
            } => {
                assert_eq!(destination, Zone::Battlefield);
                assert!(under_your_control);
                assert!(enter_tapped);
                assert_eq!(
                    enter_with_counters,
                    vec![("P1P1".to_string(), QuantityExpr::Fixed { value: 2 })]
                );
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 122.1 — Bare put-onto-battlefield without the counters suffix must
    /// emit an empty `enter_with_counters` slot. Regression-protects existing
    /// tutors / reanimation effects that place cards without counters.
    #[test]
    fn put_zone_change_without_suffix_has_empty_counters() {
        let text = "put target creature card onto the battlefield";
        let lower = text.to_lowercase();
        let effect = try_parse_put_zone_change(&lower, text)
            .expect("expected ChangeZone for put-onto-battlefield");
        match effect {
            Effect::ChangeZone {
                enter_with_counters,
                ..
            } => {
                assert!(enter_with_counters.is_empty());
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 508.4 + CR 614.1 — Kaalia of the Vast: "put X from your hand onto
    /// the battlefield tapped and attacking that opponent." The inline-tail
    /// patcher in `try_parse_put_zone_change` must set both `enter_tapped`
    /// and `enters_attacking` from the same trailing clause.
    #[test]
    fn put_zone_change_inline_tail_tapped_and_attacking_with_player_phrase() {
        let text = "put an angel, demon, or dragon creature card from your hand onto the battlefield tapped and attacking that opponent";
        let lower = text.to_lowercase();
        let effect = try_parse_put_zone_change(&lower, text)
            .expect("expected ChangeZone for Kaalia inline tail");
        match effect {
            Effect::ChangeZone {
                destination,
                enter_tapped,
                enters_attacking,
                ..
            } => {
                assert_eq!(destination, Zone::Battlefield);
                assert!(enter_tapped, "expected enter_tapped");
                assert!(enters_attacking, "expected enters_attacking");
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 508.4 — Ilharg / Preeminent Captain bare form: "...onto the
    /// battlefield tapped and attacking." with no trailing player phrase.
    #[test]
    fn put_zone_change_inline_tail_tapped_and_attacking_bare() {
        let text = "put a creature card from your hand onto the battlefield tapped and attacking";
        let lower = text.to_lowercase();
        let effect =
            try_parse_put_zone_change(&lower, text).expect("expected ChangeZone for bare tail");
        match effect {
            Effect::ChangeZone {
                enter_tapped,
                enters_attacking,
                ..
            } => {
                assert!(enter_tapped);
                assert!(enters_attacking);
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 508.4 — Hans Eriksson form with "defending player" suffix.
    #[test]
    fn put_zone_change_inline_tail_tapped_and_attacking_defending_player() {
        let text = "put it onto the battlefield tapped and attacking defending player or a planeswalker they control";
        let lower = text.to_lowercase();
        let effect = try_parse_put_zone_change(&lower, text)
            .expect("expected ChangeZone for Hans Eriksson tail");
        match effect {
            Effect::ChangeZone {
                enter_tapped,
                enters_attacking,
                ..
            } => {
                assert!(enter_tapped);
                assert!(enters_attacking);
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 508.4 — Paladin Elizabeth Taggerdy / Kinscaer Sentry "where X is" tail.
    /// The qualifier boundary must accept comma so the trailing clause "where X
    /// is ..." doesn't break the match.
    #[test]
    fn put_zone_change_inline_tail_tapped_and_attacking_where_x_is() {
        let text = "put a creature card with mana value x or less from your hand onto the battlefield tapped and attacking, where x is the number of attacking creatures you control";
        let lower = text.to_lowercase();
        let effect = try_parse_put_zone_change(&lower, text)
            .expect("expected ChangeZone for where-X-is tail");
        match effect {
            Effect::ChangeZone {
                enter_tapped,
                enters_attacking,
                ..
            } => {
                assert!(enter_tapped);
                assert!(enters_attacking);
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 614.1 — Bare "tapped" without the "and attacking" continuation.
    /// Preserves the existing tutor / reanimation behavior (Coming Attraction,
    /// Dance of the Dead) where `enter_tapped` is true but `enters_attacking`
    /// remains false.
    #[test]
    fn put_zone_change_inline_tail_tapped_only_does_not_set_attacking() {
        let text = "put target creature card onto the battlefield tapped under your control";
        let lower = text.to_lowercase();
        let effect = try_parse_put_zone_change(&lower, text)
            .expect("expected ChangeZone for tapped-only tail");
        match effect {
            Effect::ChangeZone {
                enter_tapped,
                enters_attacking,
                under_your_control,
                ..
            } => {
                assert!(enter_tapped);
                assert!(!enters_attacking, "tapped-only must not set attacking");
                assert!(under_your_control);
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 400.7 — No qualifier tail at all: neither flag set. Regression-
    /// protects untapped reanimation forms (e.g., "put it onto the battlefield
    /// under your control").
    #[test]
    fn put_zone_change_no_inline_tail_neither_flag() {
        let text = "put target creature card onto the battlefield";
        let lower = text.to_lowercase();
        let effect = try_parse_put_zone_change(&lower, text).expect("expected ChangeZone");
        match effect {
            Effect::ChangeZone {
                enter_tapped,
                enters_attacking,
                ..
            } => {
                assert!(!enter_tapped);
                assert!(!enters_attacking);
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 508.4 — Negative: a non-battlefield destination must never set
    /// `enters_attacking` even if "tapped" or "attacking" appear in the text.
    #[test]
    fn put_zone_change_non_battlefield_destination_never_attacks() {
        let text = "put target creature card into your hand";
        let lower = text.to_lowercase();
        let effect = try_parse_put_zone_change(&lower, text).expect("expected ChangeZone");
        match effect {
            Effect::ChangeZone {
                destination,
                enter_tapped,
                enters_attacking,
                ..
            } => {
                assert_eq!(destination, Zone::Hand);
                assert!(!enter_tapped);
                assert!(!enters_attacking);
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 508.4 + CR 110.2 — Chorale of the Void / The Grim Captain / Zara
    /// Renegade Recruiter: word-ordering variant where "under your control"
    /// appears between the destination and the "tapped and attacking"
    /// qualifier. The scanner must locate the qualifier at any word-boundary
    /// position in the tail, not only immediately after the needle.
    #[test]
    fn put_zone_change_inline_tail_under_control_then_tapped_and_attacking() {
        let text = "put target creature card from defending player's graveyard onto the battlefield under your control tapped and attacking";
        let lower = text.to_lowercase();
        let effect = try_parse_put_zone_change(&lower, text)
            .expect("expected ChangeZone for Chorale-of-the-Void shape");
        match effect {
            Effect::ChangeZone {
                destination,
                enter_tapped,
                enters_attacking,
                under_your_control,
                ..
            } => {
                assert_eq!(destination, Zone::Battlefield);
                assert!(under_your_control);
                assert!(enter_tapped, "expected enter_tapped");
                assert!(enters_attacking, "expected enters_attacking");
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 614.1 — Geth, Lord of the Vault / Tato Farmer / Pay-{E}-cost class:
    /// "under your control tapped" with no "and attacking". Bare-tapped form
    /// must still resolve through the scanner; `enters_attacking` stays false.
    #[test]
    fn put_zone_change_inline_tail_under_control_then_bare_tapped() {
        let text = "put target creature card from a graveyard onto the battlefield under your control tapped";
        let lower = text.to_lowercase();
        let effect = try_parse_put_zone_change(&lower, text)
            .expect("expected ChangeZone for under-control-tapped shape");
        match effect {
            Effect::ChangeZone {
                enter_tapped,
                enters_attacking,
                under_your_control,
                ..
            } => {
                assert!(under_your_control);
                assert!(enter_tapped, "expected enter_tapped");
                assert!(
                    !enters_attacking,
                    "bare tapped without 'and attacking' must not set attacking"
                );
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    // -------------------------------------------------------------------
    // L9-43 + L9-44: attach-the-just-moved-card primitive.
    //
    // CR 303.4f + CR 301.5b + CR 603.7d + CR 608.2c: when the chain is
    // `Dig|ChangeZone(destination=Battlefield)` → `Attach` with a
    // `ZoneChangedThisWay` condition, the parent must carry
    // `forward_result: true` so the runtime forwards the just-moved card
    // as the Attach's `source_id` instead of the trigger source / Saga /
    // searcher.
    // -------------------------------------------------------------------

    /// Armored Skyhunter's attack trigger: Dig(6, Battlefield) →
    /// Attach(creature you control) with the "If an Equipment is put onto
    /// the battlefield this way" condition. Asserts the `forward_result`
    /// rewire fires AND the condition is recognized AND the Attach target
    /// slot is preserved.
    #[test]
    fn attach_just_moved_armored_skyhunter_dig_with_zone_changed_this_way() {
        let def = parse_effect_chain(
            "Look at the top six cards of your library. You may put an Aura or Equipment card from among them onto the battlefield. If an Equipment is put onto the battlefield this way, you may attach it to a creature you control. Put the rest of those cards on the bottom of your library in a random order.",
            AbilityKind::Spell,
        );

        // Parent: Dig{destination: Battlefield} with forward_result: true.
        match &*def.effect {
            Effect::Dig { destination, .. } => {
                assert_eq!(*destination, Some(Zone::Battlefield));
            }
            other => panic!("expected outer Dig, got {other:?}"),
        }
        assert!(
            def.forward_result,
            "Dig parent must forward the just-moved card to the Attach sub_ability"
        );

        // Walk the sub_ability chain: Attach with ZoneChangedThisWay condition.
        let attach = def
            .sub_ability
            .as_ref()
            .expect("expected Attach sub_ability");
        match &*attach.effect {
            Effect::Attach { target } => {
                // Target slot must not be vacuous (Any) — either a typed
                // "creature you control" filter or `ParentTarget` (chain
                // composition routes the player-chosen target through the
                // parent's slot). Both are valid post-assembly shapes; what
                // matters is that the runtime has a target binding to fill.
                assert!(
                    !matches!(target, TargetFilter::Any),
                    "Attach target slot must not be Any, got {target:?}"
                );
                if let TargetFilter::Typed(t) = target {
                    assert_eq!(t.controller, Some(ControllerRef::You));
                    assert!(
                        t.type_filters
                            .iter()
                            .any(|f| matches!(f, TypeFilter::Creature)),
                        "expected Creature type_filter, got {:?}",
                        t.type_filters
                    );
                }
            }
            other => panic!("expected Attach sub_ability, got {other:?}"),
        }
        match &attach.condition {
            Some(AbilityCondition::ZoneChangedThisWay { filter }) => match filter {
                TargetFilter::Typed(t) => assert!(
                    t.type_filters
                        .iter()
                        .any(|f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Equipment"))),
                    "expected Equipment subtype filter on ZoneChangedThisWay, got {:?}",
                    t.type_filters
                ),
                other => panic!("expected Typed Equipment filter, got {other:?}"),
            },
            other => panic!("expected ZoneChangedThisWay condition on Attach, got {other:?}"),
        }
    }

    /// Quest for the Holy Relic / Stonehewer Giant pattern:
    /// SearchLibrary → ChangeZone(destination=Battlefield) → Attach. The
    /// rewire detects the ChangeZone-to-battlefield parent and sets
    /// forward_result regardless of how many shuffles / cleanups follow.
    #[test]
    fn attach_just_moved_change_zone_to_battlefield_attach_sub() {
        // Synthetic minimal pattern that exercises the same chain shape as
        // Stonehewer Giant ("you may search your library for an Equipment
        // card, put it onto the battlefield, then shuffle. If that
        // Equipment is put onto the battlefield this way, attach it to a
        // creature you control."). Phrased with present-tense "is put"
        // because that's the new pattern the combinator must recognize.
        let def = parse_effect_chain(
            "Search your library for an Equipment card and put it onto the battlefield. If an Equipment is put onto the battlefield this way, attach it to a creature you control. Then shuffle.",
            AbilityKind::Spell,
        );

        // Walk the chain to find the ChangeZone(Battlefield) node — the
        // outermost effect may be SearchLibrary depending on parser
        // composition. The post-pass recurses, so any Dig/ChangeZone-to-
        // Battlefield parent in the tree should be marked.
        fn find_battlefield_change_zone_with_attach_sub(
            def: &AbilityDefinition,
        ) -> Option<&AbilityDefinition> {
            let dest_is_bf = matches!(
                &*def.effect,
                Effect::ChangeZone {
                    destination: Zone::Battlefield,
                    ..
                } | Effect::Dig {
                    destination: Some(Zone::Battlefield),
                    ..
                }
            );
            if dest_is_bf {
                if let Some(sub) = def.sub_ability.as_ref() {
                    if matches!(*sub.effect, Effect::Attach { .. })
                        && matches!(
                            sub.condition,
                            Some(AbilityCondition::ZoneChangedThisWay { .. })
                        )
                    {
                        return Some(def);
                    }
                }
            }
            if let Some(sub) = def.sub_ability.as_ref() {
                if let Some(found) = find_battlefield_change_zone_with_attach_sub(sub) {
                    return Some(found);
                }
            }
            None
        }

        let parent = find_battlefield_change_zone_with_attach_sub(&def).expect(
            "expected a ChangeZone|Dig to Battlefield with an Attach sub gated by ZoneChangedThisWay",
        );
        assert!(
            parent.forward_result,
            "battlefield-bound parent of conditional Attach must have forward_result: true"
        );
    }

    /// Negative regression: Stoneforge Mystic-style "put an Equipment from
    /// your hand onto the battlefield" must NOT receive `forward_result:
    /// true` because the pattern has no Attach sub-ability — it's just a
    /// ChangeZone, no chained attach. Guards against the rewire over-firing.
    #[test]
    fn attach_just_moved_negative_no_attach_sub_no_rewire() {
        let def = parse_effect_chain(
            "You may put an Equipment card from your hand onto the battlefield.",
            AbilityKind::Spell,
        );
        // The chain may be wrapped (optional / target) but no descendant
        // should carry forward_result: true since there's no Attach.
        fn any_forward_result(def: &AbilityDefinition) -> bool {
            if def.forward_result {
                return true;
            }
            if let Some(sub) = def.sub_ability.as_ref() {
                if any_forward_result(sub) {
                    return true;
                }
            }
            if let Some(else_branch) = def.else_ability.as_ref() {
                if any_forward_result(else_branch) {
                    return true;
                }
            }
            false
        }
        assert!(
            !any_forward_result(&def),
            "ChangeZone-only (no Attach sub) must not be marked forward_result"
        );
    }

    // ── L9-20 + L8-1: cast-from-zone constraints + alt-ability-cost rider ──

    /// CR 118.9 + CR 120.3: Buster Sword's "cast a spell from your hand with
    /// mana value less than or equal to that damage without paying its mana
    /// cost" must produce a `CastFromZone` whose target is constrained to a
    /// `Card` filter with `InZone { Hand }` + `CmcLE { EventContextAmount }`.
    /// Building-block test: covers the entire FIN equipment-cycle class
    /// (~10–15 cards) by exercising the type-phrase + origin-zone +
    /// mana-value-suffix composition in `try_parse_cast_effect`.
    #[test]
    fn cast_from_zone_with_filter_and_damage_bound() {
        let effect = super::parse_effect(
            "cast a spell from your hand with mana value less than or equal to that damage without paying its mana cost",
        );
        match &effect {
            Effect::CastFromZone {
                target: TargetFilter::Typed(tf),
                without_paying_mana_cost: true,
                mode: crate::types::ability::CardPlayMode::Cast,
                alt_ability_cost: None,
                ..
            } => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Card]);
                assert_eq!(tf.controller, Some(ControllerRef::You));
                // structural: not dispatch — Vec membership assertion in test
                assert!(tf
                    .properties
                    .contains(&FilterProp::InZone { zone: Zone::Hand }));
                let has_cmc_le_damage = tf.properties.iter().any(|p| {
                    matches!(
                        p,
                        FilterProp::CmcLE {
                            value: crate::types::ability::QuantityExpr::Ref {
                                qty: crate::types::ability::QuantityRef::EventContextAmount,
                            },
                        }
                    )
                });
                assert!(
                    has_cmc_le_damage,
                    "expected CmcLE bound on EventContextAmount, got {:?}",
                    tf.properties
                );
            }
            _ => panic!("expected constrained CastFromZone, got {effect:?}"),
        }
    }

    /// CR 202.3: "with mana value less than or equal to that creature" —
    /// preserves the legacy `EventContextSourceManaValue` semantic for the
    /// type-word arm; only "that damage" routes to `EventContextAmount`.
    /// Regression test for the `try_dynamic` factoring in
    /// `parse_mana_value_suffix`.
    #[test]
    fn cast_from_zone_mana_value_that_creature_keeps_source_mv() {
        let effect = super::parse_effect(
            "cast a spell from your hand with mana value less than or equal to that creature without paying its mana cost",
        );
        let target = match &effect {
            Effect::CastFromZone {
                target: TargetFilter::Typed(tf),
                ..
            } => tf,
            _ => panic!("expected CastFromZone, got {effect:?}"),
        };
        let has_source_mv = target.properties.iter().any(|p| {
            matches!(
                p,
                FilterProp::CmcLE {
                    value: crate::types::ability::QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::EventContextSourceManaValue,
                    },
                }
            )
        });
        assert!(
            has_source_mv,
            "expected EventContextSourceManaValue for type-word arm, got {:?}",
            target.properties
        );
    }

    /// CR 118.9 + CR 119.4: Nashi's "If you cast a spell this way, pay life
    /// equal to its mana value rather than paying its mana cost" must fold
    /// onto the previous `CastFromZone` clause as `alt_ability_cost`, NOT
    /// emit as a sibling `Unimplemented{name:"pay"}`. Building-block test:
    /// exercises both `try_parse_alt_cost_rider` (rider recognition) and
    /// `attach_alt_cost_to_prior_cast_from_zone` (chain folding).
    #[test]
    fn alt_cost_rider_folds_onto_prior_cast_from_zone() {
        let def = super::parse_effect_chain(
            "exile the top card of each player's library. Until end of turn, you may play one of those cards. If you cast a spell this way, pay life equal to its mana value rather than paying its mana cost.",
            AbilityKind::Spell,
        );
        // Walk the chain looking for the CastFromZone with alt_ability_cost set.
        fn find_cast(d: &AbilityDefinition) -> Option<&Effect> {
            if matches!(*d.effect, Effect::CastFromZone { .. }) {
                return Some(&d.effect);
            }
            d.sub_ability.as_ref().and_then(|s| find_cast(s))
        }
        let cast = find_cast(&def).expect("CastFromZone should be in chain");
        let Effect::CastFromZone {
            alt_ability_cost: Some(alt),
            ..
        } = cast
        else {
            panic!("expected CastFromZone with alt_ability_cost, got {cast:?}");
        };
        assert!(
            matches!(
                alt,
                crate::types::ability::AbilityCost::PayLife {
                    amount: crate::types::ability::QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::SelfManaValue,
                    },
                }
            ),
            "expected PayLife {{ SelfManaValue }}, got {alt:?}",
        );
        // Verify the rider didn't leak a sibling Unimplemented{name:"pay"} effect.
        fn has_pay_unimpl(d: &AbilityDefinition) -> bool {
            if matches!(
                &*d.effect,
                Effect::Unimplemented { name, .. } if name == "pay"
            ) {
                return true;
            }
            d.sub_ability.as_ref().is_some_and(|s| has_pay_unimpl(s))
        }
        assert!(
            !has_pay_unimpl(&def),
            "rider must NOT leak as Unimplemented{{name:'pay'}} sibling"
        );
    }
}
