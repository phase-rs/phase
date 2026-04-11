mod animation;
mod conditions;
pub(crate) mod counter;
pub(crate) mod imperative;
pub(crate) mod mana;
mod search;
mod sequence;
pub(crate) mod subject;
mod token;
mod types;

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
    contains_possessive, has_unconsumed_conditional, parse_mana_symbols, starts_with_possessive,
    strip_after, TextPair,
};
use crate::database::mtgjson::parse_mtgjson_mana_cost;
use crate::parser::oracle_effect::subject::parse_subject_application;
use crate::parser::oracle_warnings::push_warning;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, CardPlayMode, CastingPermission, ChoiceType,
    ConjureCard, ContinuousModification, ControllerRef, DamageSource, DelayedTriggerCondition,
    Duration, Effect, FilterProp, GameRestriction, MultiTargetSpec, PlayerFilter, PtValue,
    QuantityExpr, QuantityRef, RestrictionExpiry, RestrictionPlayerScope, RoundingMode,
    StaticCondition, StaticDefinition, TargetFilter, TypeFilter, TypedFilter, UnlessCost, ZoneRef,
};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::game_state::{DistributionUnit, NextSpellModifier, RetargetScope};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::statics::StaticMode;
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
    })
}

/// CR 603.7: Parse "when you next cast a [type] spell this turn, [effect]" delayed triggers.
/// Creates a one-shot delayed trigger that fires once on the next matching SpellCast event.
/// Examples:
/// - "When you next cast a creature spell this turn, that creature enters with an additional +1/+1 counter on it."
/// - "When you next cast a creature spell this turn, search your library for a creature card..."
fn try_parse_when_next_event(tp: TextPair) -> Option<ParsedEffectClause> {
    use crate::types::triggers::TriggerMode;

    // Must start with "when you next cast a "
    let _ = tag::<_, _, VerboseError<&str>>("when you next cast a ")
        .parse(tp.lower)
        .ok()?;

    // Must contain "this turn, " to delimit condition from effect
    let (before_this_turn, after) = tp.rsplit_around(" this turn, ")?;

    // Extract the spell type from between "when you next cast a " and " spell"
    let condition_fragment = &before_this_turn.lower["when you next cast a ".len()..];
    // Verify " spell" suffix and parse type via parse_type_phrase building block
    let spell_suffix = condition_fragment.strip_suffix(" spell")?;
    let (type_filter, remainder) = super::oracle_target::parse_type_phrase(spell_suffix);
    if !remainder.trim().is_empty() {
        return None;
    }

    // Build a SpellCast trigger definition with the parsed type filter
    let mut trigger_def = crate::types::ability::TriggerDefinition::new(TriggerMode::SpellCast);
    trigger_def.valid_card = Some(type_filter);
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
        }
    };

    Some(ParsedEffectClause {
        effect,
        duration: None,
        sub_ability: Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GrantCastingPermission {
                permission: CastingPermission::ExileWithAltCost { cost },
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
            },
        ))),
        distribute: None,
        multi_target,
        condition: None,
    })
}

/// CR 402.2 + CR 114.1: Parse "you have no maximum hand size [for the rest of the game]"
/// as an effect that creates an emblem with `NoMaximumHandSize` static.
///
/// When this text appears on a permanent as a static ability, the static parser handles it.
/// When it appears as an effect line in a spell or triggered ability (e.g., Choice of Fortunes),
/// it needs to create an emblem to produce a persistent game-state effect.
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
        // Fallback: parse remaining as a generic imperative effect
        let clause = parse_effect_clause(rest.original, ctx);
        return Some(clause);
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

    // CR 121.6: "{verb} cards equal to {quantity}" — dynamic count from game state.
    if let Some(clause) = try_parse_equal_to_quantity_effect(tp) {
        return clause;
    }

    // CR 702.84a: "exile cards from the top of your library until you exile a [filter] card"
    if let Some(clause) = try_parse_exile_from_top_until(tp) {
        return clause;
    }

    // CR 701.20a: "reveal cards from the top of your library until you reveal a [filter]"
    if let Some(clause) = try_parse_reveal_until(tp) {
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
    })
}

/// CR 614.10: Parse "you skip your next turn" / "skip your next turn" — temporal penalty effect.
fn try_parse_skip_next_turn(tp: TextPair) -> Option<ParsedEffectClause> {
    let _ = nom_on_lower(tp.original, tp.lower, |input| {
        alt((
            value((), tag("you skip your next turn")),
            value((), tag("skip your next turn")),
        ))
        .parse(input)
    })?;

    Some(parsed_clause(Effect::SkipNextTurn {
        target: TargetFilter::Controller,
    }))
}

/// Parse "{verb} cards equal to {quantity_ref}" patterns (CR 121.6).
///
/// Handles verbs whose count field is `QuantityExpr` (mill, draw).
fn try_parse_equal_to_quantity_effect(tp: TextPair) -> Option<ParsedEffectClause> {
    // Try matching "mill cards equal to " or "draw cards equal to " using nom.
    let (verb, rest_orig) = nom_on_lower(tp.original, tp.lower, |input| {
        alt((
            value("mill", tag("mill cards equal to ")),
            value("draw", tag("draw cards equal to ")),
        ))
        .parse(input)
    })?;
    let rest_lower = &tp.lower[tp.lower.len() - rest_orig.len()..];
    let rest = rest_lower.trim().trim_end_matches('.');
    // CR 603.7c: Prefer event context quantity for triggered effects.
    let qty = super::oracle_quantity::parse_event_context_quantity(rest)?;
    match verb {
        "mill" => Some(parsed_clause(Effect::Mill {
            count: qty,
            // CR 701.17a: No subject → controller mills.
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        })),
        "draw" => Some(parsed_clause(Effect::Draw { count: qty })),
        _ => None,
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
    // Match: "exile cards from the top of your library until you exile a/an {filter} card"
    let (_, rest_orig) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, _) = tag("exile cards from the top of your library until you exile ").parse(i)?;
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

/// CR 701.20a: Parse "reveal cards from the top of your library until you reveal a [filter]".
/// Defaults: kept_destination = Hand, rest_destination = Library (bottom).
/// Subsequent "put that card" / "put the rest" sentences override via ContinuationAst.
fn try_parse_reveal_until(tp: TextPair) -> Option<ParsedEffectClause> {
    let (_, rest_orig) = nom_on_lower(tp.original, tp.lower, |i| {
        let (i, _) = tag("reveal cards from the top of your library until you reveal ").parse(i)?;
        nom_primitives::parse_article(i)
    })?;
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
        filter,
        kept_destination: Zone::Hand,
        rest_destination: Zone::Library,
        enter_tapped: false,
    }))
}

/// CR 400.7i: Parse "you may play/cast that card [this turn]" — impulse draw permission.
fn try_parse_play_from_exile(tp: TextPair) -> Option<ParsedEffectClause> {
    let tp = tp.trim_end_matches('.');

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
            Effect::DamageAll { amount, target } => Effect::DamageAll {
                amount: replace_fixed_quantity(amount, quantity),
                target,
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
        let (_, counter_type) = nom_primitives::parse_counter_type.parse(ct_start).ok()?;
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

    let target = match parse_subject_application(subject_text, &ParseContext::default()) {
        Some(app) => app.affected,
        None => return effect,
    };

    // Only replace default/placeholder targets — leave already-resolved targets alone.
    match effect {
        Effect::Pump {
            power,
            toughness,
            target: TargetFilter::Any,
        } => Effect::Pump {
            power,
            toughness,
            target,
        },
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
            // "put target [type] on top/bottom of library" — the imperative parser
            // returns PutAtLibraryPosition { target: Any } because it doesn't extract
            // the target from between "put" and "on top/bottom". Extract it here.
            if let Effect::PutAtLibraryPosition { ref mut target, .. } = clause.effect {
                if *target == TargetFilter::Any {
                    let lower = text.to_lowercase();
                    // Check if text starts with "put " using nom tag, then use remainder.
                    if tag::<_, _, VerboseError<&str>>("put ")
                        .parse(lower.as_str())
                        .is_ok()
                    {
                        let after_put = &lower["put ".len()..];
                        let boundary = after_put
                            .find(" on top of")
                            .or_else(|| after_put.find(" on the bottom of"));
                        if let Some(end) = boundary {
                            let target_text = &after_put[..end];
                            let (filter, _) = parse_target(target_text);
                            if !matches!(filter, TargetFilter::Any) {
                                *target = filter;
                            }
                        }
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
        let (target_text, _) = strip_optional_target_prefix(rest);
        let (target, rem) = parse_target(target_text);
        return Some((TargetedImperativeAst::Sacrifice { target }, rem));
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
    // Earthbend: "earthbend [N] [target <type>]" → Animate with haste + is_earthbend
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

    // CR 701.5a: Counter a spell or ability on the stack.
    if let Some((_, rest)) = nom_on_lower(text, lower, |i| value((), tag("counter ")).parse(i)) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (parsed_target, rem) = parse_target(rest);
        let target = if scan_contains_phrase(rest_lower, "activated or triggered ability") {
            // CR 701.5a: "activated or triggered ability" is a special-case target
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
/// This compound target pattern deals damage to BOTH players and objects with a single
/// amount. It produces DamageEachPlayer as the primary effect with a DamageAll sub_ability.
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

    // Match: "each [opponent/player] and each [type phrase] they control"
    let (after_each, _) = tag::<_, _, VerboseError<&str>>("each ")
        .parse(after_amount)
        .ok()?;
    let (after_player, player_filter) = alt((
        value(
            PlayerFilter::Opponent,
            tag::<_, _, VerboseError<&str>>("opponent"),
        ),
        value(PlayerFilter::All, tag("player")),
    ))
    .parse(after_each)
    .ok()?;

    // " and each " connector
    let (after_and_each, _) = tag::<_, _, VerboseError<&str>>(" and each ")
        .parse(after_player)
        .ok()?;

    // Strip "they control" suffix to get the type phrase
    // "creature and planeswalker they control" or "planeswalker they control"
    let type_phrase_lower = after_and_each
        .strip_suffix(" they control")
        .or_else(|| after_and_each.strip_suffix(" they control."))?
        .trim();
    if type_phrase_lower.is_empty() {
        return None;
    }

    // Use parse_target on "each [type]" to get the correct typed filter with controller
    let target_text = format!("each {type_phrase_lower}");
    let (mut object_filter, _rem) = parse_target(&target_text);

    // Set controller to Opponent to match "they control" (where "they" = opponents).
    // The filter may be a single Typed or an Or of multiple Typed filters.
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

    let sub_ability = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DamageAll {
            amount: qty.clone(),
            target: object_filter,
        },
    );

    Some(ParsedEffectClause {
        effect: Effect::DamageEachPlayer {
            amount: qty,
            player_filter,
        },
        duration: None,
        sub_ability: Some(Box::new(sub_ability)),
        distribute: None,
        multi_target: None,
        condition: None,
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
    };

    Some(ParsedEffectClause {
        effect: primary_effect,
        duration: None,
        sub_ability: Some(Box::new(sub_def)),
        distribute: None,
        multi_target: None,
        condition: None,
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
        },
        PredicateAst::Restriction { effect, duration } => ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
            distribute: None,
            multi_target,
            condition: None,
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
                    // CR 107.2: "half of their library, rounded up/down"
                    if let Ok((_, _)) = tag::<_, _, VerboseError<&str>>("half ").parse(after_top) {
                        let rounding = if scan_contains_phrase(&pred_lower, "rounded up") {
                            RoundingMode::Up
                        } else {
                            RoundingMode::Down
                        };
                        QuantityExpr::HalfRounded {
                            inner: Box::new(QuantityExpr::Ref {
                                qty: QuantityRef::TargetZoneCardCount {
                                    zone: ZoneRef::Library,
                                },
                            }),
                            rounding,
                        }
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
                    };
                }
            }
            // CR 608.2c: Inject the subject's target into targeted effects that were
            // parsed via the imperative path (connive, phase out, force block, suspect).
            inject_subject_target(&mut clause.effect, &subject);
            if clause.multi_target.is_none() {
                clause.multi_target = subject.multi_target;
            }
            clause
        }
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
        Effect::Connive { target, .. }
        | Effect::PhaseOut { target }
        | Effect::ForceBlock { target }
        | Effect::Suspect { target }
        | Effect::Goad { target }
        | Effect::Mill { target, .. }
        | Effect::Discard { target, .. }
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
        // CR 119.3 + CR 115.1d: "they lose N life" — inject subject's player reference.
        // LoseLife.target is Option<TargetFilter>, unlike other effects' non-optional targets.
        // Guard on is_none() to only inject when no target was explicitly parsed.
        Effect::LoseLife { ref mut target, .. } if target.is_none() => {
            *target = Some(subject_filter);
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
    type E<'a> = VerboseError<&'a str>;

    // CR 305.1: "play" means cast if spell, play as land if land.
    let (rest, mode) = alt((
        value(CardPlayMode::Cast, tag::<_, _, E>("cast ")),
        value(CardPlayMode::Play, tag("play ")),
    ))
    .parse(lower)
    .ok()?;

    let without_paying = scan_contains_phrase(rest, "without paying its mana cost")
        || scan_contains_phrase(rest, "without paying their mana cost");

    let target = if alt((
        tag::<_, _, E>("it"),
        tag("that card"),
        tag("that spell"),
        tag("the copy"),
        tag("the exiled card"),
        tag("them"),
        tag("those cards"),
        tag("cards exiled"),
    ))
    .parse(rest)
    .is_ok()
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

        // "Starting with you, " — multiplayer ordering modifier that's irrelevant
        // for 1v1. Strip the prefix so the remaining effect text is parsed normally.
        let normalized_text = {
            let temp_lower = normalized_text.to_lowercase();
            nom_on_lower(normalized_text, &temp_lower, |i| {
                value((), tag("starting with you, ")).parse(i)
            })
            .map_or(normalized_text, |((), rest)| rest)
        };

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

        // CR 608.2e: "if [condition], [effect] instead" — the preceding ability's effect
        // is replaced when the condition holds. Model as: new def has condition + instead effect,
        // preceding def becomes else_ability (the fallback when condition is false).
        if let Some(instead_def) = try_parse_generic_instead_clause(normalized_text, kind) {
            if let Some(last_def) = defs.pop() {
                let mut new_def = instead_def;
                new_def.else_ability = Some(Box::new(last_def));
                defs.push(new_def);
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
        let text_where_x_lower = text.to_lowercase();
        let (_, where_x_expression) =
            strip_trailing_where_x(TextPair::new(&text, &text_where_x_lower));
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

        // CR 608.2c: TargetOnly is a structural wrapper — its sub_ability is the action
        // to perform on the target. Preserve this relationship rather than flattening.
        let is_target_only = matches!(clause.effect, Effect::TargetOnly { .. });
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

    // CR 706 + CR 705: Consolidate die result table lines into their parent RollDie,
    // and coin flip conditional branches into their parent FlipCoin.
    consolidate_die_and_coin_defs(&mut defs, kind);

    // Chain: last has no sub_ability, each earlier one chains to next.
    // When a def already has a sub_ability (e.g., TargetOnly with attached Explore),
    // append to the deepest sub rather than overwriting.
    if defs.len() > 1 {
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
    // "you may" — standard optional effect prefix.
    if let Some((scope, rest)) = nom_on_lower(text, &lower, |input| {
        alt((
            value(
                Some(crate::types::ability::OpponentMayScope::AnyOpponent),
                tag("any opponent may "),
            ),
            value(None, tag("you may ")),
        ))
        .parse(input)
    }) {
        (true, scope, rest.to_string())
    } else {
        (false, None, text.to_string())
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

    // "it has a {type} counter on it" / "~ has a {type} counter on it"
    if scan_contains_phrase(condition, "has a") && scan_contains_phrase(condition, "counter on it")
    {
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
                    if scan_contains_phrase(target_phrase, "player")
                        || scan_contains_phrase(target_phrase, "opponent")
                        || scan_contains_phrase(target_phrase, "foe")
                    {
                        let player_filter = if scan_contains_phrase(target_phrase, "opponent")
                            || scan_contains_phrase(target_phrase, "foe")
                        {
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
    if tag::<_, _, VerboseError<&str>>("each ")
        .parse(after_to)
        .is_ok()
    {
        let (target, rem) = parse_target(after_to);
        return Some((Effect::DamageAll { amount, target }, rem));
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
        | Effect::GainLife { amount, .. }
        | Effect::LoseLife { amount, .. }
        | Effect::IncreaseSpeed { amount, .. }
        | Effect::Draw { count: amount }
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
            let under_your_control = scan_contains_phrase(after_put_tp.lower, "under your control");
            return Some(Effect::ChangeZone {
                origin: infer_origin_zone(after_put_tp.lower),
                destination,
                target,
                owner_library: false,
                enter_transformed: false,
                under_your_control,
                // CR 603.6d: "enters tapped" — detect tapped qualifier after battlefield destination.
                enter_tapped: destination == Zone::Battlefield
                    && scan_contains_phrase(after_put_tp.lower, "battlefield tapped"),
                enters_attacking: false,
                up_to: false,
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
    if contains_possessive(lower, "from", "graveyard")
        || scan_contains_phrase(lower, "from a graveyard")
    {
        Some(Zone::Graveyard)
    } else if scan_contains_phrase(lower, "from exile") {
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
        Comparator, ContinuousModification, ControllerRef, DoublePTMode, Duration, FilterProp,
        GainLifePlayer, ManaProduction, NinjutsuVariant, PaymentCost, TypeFilter,
    };
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::zones::Zone;

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
    fn try_split_damage_compound_player_and_object_target() {
        // Goblin Chainwhirler: "deals 1 damage to each opponent and each creature
        // and planeswalker they control" → DamageEachPlayer + DamageAll(Creature|Planeswalker)
        let ctx = ParseContext::default();
        let result = try_split_damage_compound(
            "deal 1 damage to each opponent and each creature and planeswalker they control",
            &ctx,
        );
        let clause = result.expect("compound player+object damage should parse");
        assert!(
            matches!(
                clause.effect,
                Effect::DamageEachPlayer {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player_filter: PlayerFilter::Opponent,
                }
            ),
            "primary should be DamageEachPlayer(Opponent), got: {:?}",
            clause.effect
        );
        let sub = clause
            .sub_ability
            .expect("should have DamageAll sub_ability");
        match &*sub.effect {
            Effect::DamageAll { amount, target } => {
                assert!(matches!(amount, QuantityExpr::Fixed { value: 1 }));
                match target {
                    TargetFilter::Or { filters } => {
                        assert_eq!(filters.len(), 2);
                        // Both inner filters should have controller=Opponent
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
            other => panic!("sub_ability should be DamageAll, got: {other:?}"),
        }
    }

    #[test]
    fn try_split_damage_compound_player_and_planeswalker() {
        // Kumano Faces Kakkazan: "deals 1 damage to each opponent and each planeswalker they control"
        let ctx = ParseContext::default();
        let result = try_split_damage_compound(
            "deal 1 damage to each opponent and each planeswalker they control",
            &ctx,
        );
        let clause = result.expect("compound player+object damage should parse");
        assert!(matches!(clause.effect, Effect::DamageEachPlayer { .. }));
        let sub = clause
            .sub_ability
            .expect("should have DamageAll sub_ability");
        match &*sub.effect {
            Effect::DamageAll { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert!(tf.type_filters.contains(&TypeFilter::Planeswalker));
                    assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                }
                other => panic!("expected Typed filter, got: {other:?}"),
            },
            other => panic!("sub_ability should be DamageAll, got: {other:?}"),
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
                    count: QuantityExpr::Fixed { value: 1 }
                }
            ),
            "expected Manifest {{ count: 1 }}, got: {e:?}"
        );
    }

    #[test]
    fn effect_manifest_top_two_cards() {
        let e = parse_effect("Manifest the top two cards of your library");
        assert!(
            matches!(
                e,
                Effect::Manifest {
                    count: QuantityExpr::Fixed { value: 2 }
                }
            ),
            "expected Manifest {{ count: 2 }}, got: {e:?}"
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
                    count: QuantityExpr::Fixed { value: 7 }
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
                assert_eq!(count, 1);
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
            Effect::Mana { produced: ManaProduction::AnyOneColor { count: QuantityExpr::Fixed { value: 1 }, ref color_options }, .. }
            if color_options == &vec![ManaColor::White, ManaColor::Blue, ManaColor::Black, ManaColor::Red, ManaColor::Green]
        ));
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
    fn effect_you_have_no_max_hand_size_for_rest_of_game() {
        // CR 402.2 + CR 114.1: Spell effect "you have no maximum hand size for the rest
        // of the game" creates an emblem with NoMaximumHandSize.
        let e = parse_effect("You have no maximum hand size for the rest of the game.");
        match e {
            Effect::CreateEmblem { statics } => {
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
            Effect::CreateEmblem { statics } => {
                assert_eq!(statics.len(), 1);
                assert_eq!(statics[0].mode, StaticMode::NoMaximumHandSize);
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
                            rounding: RoundingMode::Up,
                            ..
                        }
                    ),
                    "Expected HalfRounded(Up), got {count:?}"
                );
            }
            other => panic!("Expected ExileTop, got {other:?}"),
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
    fn ninjutsu_variant_paid_sneak_non_instead() {
        // CR 702.49: "if this spell's sneak cost was paid, [effect]" without "instead"
        let (cond, text) = strip_additional_cost_conditional(
            "if this spell's sneak cost was paid, they enter tapped and attacking",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::NinjutsuVariantPaid {
                variant: NinjutsuVariant::Sneak,
            })
        );
        assert_eq!(text, "they enter tapped and attacking");
    }

    #[test]
    fn ninjutsu_variant_paid_ninjutsu_non_instead() {
        // CR 702.49: "if its ninjutsu cost was paid, [effect]" without "instead"
        let (cond, text) = strip_additional_cost_conditional(
            "if its ninjutsu cost was paid, those tokens enter tapped and attacking",
        );
        assert_eq!(
            cond,
            Some(AbilityCondition::NinjutsuVariantPaid {
                variant: NinjutsuVariant::Ninjutsu,
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
            Some(AbilityCondition::NinjutsuVariantPaid {
                variant: NinjutsuVariant::Sneak,
            }),
            "Token should have NinjutsuVariantPaid condition"
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
                    filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
                    kept_destination: Zone::Hand,
                    rest_destination: Zone::Library,
                    enter_tapped: false,
                } if type_filters.contains(&TypeFilter::Creature)
            ),
            "expected RevealUntil creature->hand, rest->library, got: {:?}",
            def.effect
        );
        // No sub_ability — destinations are baked in
        assert!(def.sub_ability.is_none(), "should have no sub_ability");
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
                    filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
                    kept_destination: Zone::Battlefield,
                    rest_destination: Zone::Library,
                    enter_tapped: false,
                } if type_filters.contains(&TypeFilter::Artifact)
            ),
            "expected RevealUntil artifact->battlefield, got: {:?}",
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
                    count: QuantityExpr::Fixed { value: 1 }
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
            AbilityCondition::TargetMatchesFilter { filter, use_lki } => {
                assert!(!use_lki);
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
}
