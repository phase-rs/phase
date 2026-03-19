use super::animation::{animation_modifications, parse_animation_spec};
use super::types::*;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Duration, Effect, GainLifePlayer, PtValue, QuantityExpr,
    QuantityRef, StaticDefinition, TargetFilter,
};
use crate::types::statics::StaticMode;

use super::super::oracle_static::parse_continuous_modifications;
use super::super::oracle_target::{parse_target, parse_type_phrase};
use super::super::oracle_util::parse_number;

pub(super) fn try_parse_subject_predicate_ast(text: &str) -> Option<ClauseAst> {
    if try_parse_targeted_controller_gain_life(text).is_some() {
        return None;
    }

    if let Some(clause) = try_parse_subject_continuous_clause(text) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, sub_ability| PredicateAst::Continuous {
                effect,
                duration,
                sub_ability,
            },
        ));
    }

    if let Some(clause) = try_parse_subject_become_clause(text) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, _sub_ability| PredicateAst::Become { effect, duration },
        ));
    }

    if let Some(clause) = try_parse_subject_restriction_clause(text) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, _sub_ability| PredicateAst::Restriction { effect, duration },
        ));
    }

    if let Some(stripped) = strip_subject_clause(text) {
        let subject_text = extract_subject_text(text)?;
        let application = parse_subject_application(&subject_text).unwrap_or(SubjectApplication {
            affected: TargetFilter::Any,
            target: None,
        });
        return Some(ClauseAst::SubjectPredicate {
            subject: SubjectPhraseAst {
                affected: application.affected,
                target: application.target,
            },
            predicate: Box::new(PredicateAst::ImperativeFallback { text: stripped }),
        });
    }

    None
}

fn subject_predicate_ast_from_clause<F>(
    text: &str,
    clause: ParsedEffectClause,
    build_predicate: F,
) -> ClauseAst
where
    F: FnOnce(Effect, Option<Duration>, Option<Box<AbilityDefinition>>) -> PredicateAst,
{
    let subject_text = extract_subject_text(text).unwrap_or_default();
    let application = parse_subject_application(&subject_text).unwrap_or(SubjectApplication {
        affected: TargetFilter::Any,
        target: None,
    });

    ClauseAst::SubjectPredicate {
        subject: SubjectPhraseAst {
            affected: application.affected,
            target: application.target,
        },
        predicate: Box::new(build_predicate(
            clause.effect,
            clause.duration,
            clause.sub_ability,
        )),
    }
}

fn extract_subject_text(text: &str) -> Option<String> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    if subject.is_empty() {
        None
    } else {
        Some(subject.to_string())
    }
}

fn try_parse_subject_continuous_clause(text: &str) -> Option<ParsedEffectClause> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = text[verb_start..].trim();
    let application = parse_subject_application(subject)?;
    build_continuous_clause(application, predicate)
}

fn try_parse_subject_become_clause(text: &str) -> Option<ParsedEffectClause> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = deconjugate_verb(text[verb_start..].trim());
    if !predicate.to_lowercase().starts_with("become ") {
        return None;
    }
    let application = parse_subject_application(subject)?;
    build_become_clause(application, &predicate)
}

fn try_parse_subject_restriction_clause(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    let (subject, predicate) = if let Some(pos) = lower.find(" can't ") {
        (text[..pos].trim(), text[pos + 1..].trim())
    } else if let Some(pos) = lower.find(" cannot ") {
        (text[..pos].trim(), text[pos + 1..].trim())
    } else {
        return None;
    };
    let application = parse_subject_application(subject)?;
    build_restriction_clause(application, predicate)
}

fn parse_subject_application(subject: &str) -> Option<SubjectApplication> {
    if subject.trim().is_empty() {
        return None;
    }

    let lower = subject.to_lowercase();

    if lower.starts_with("target ") {
        let (filter, _) = parse_target(subject);
        return subject_filter_application(filter, true);
    }
    // "up to N target X" — the quantifier count is handled by strip_any_number_quantifier()
    // in parse_effect_chain; here we just need to route to parse_target for the filter.
    if lower.starts_with("up to ") {
        if let Some(target_pos) = lower.find("target ") {
            let (filter, _) = parse_target(&subject[target_pos..]);
            return subject_filter_application(filter, true);
        }
    }
    if lower.starts_with("all ") || lower.starts_with("each ") {
        let (filter, _) = parse_target(subject);
        return subject_filter_application(filter, false);
    }
    if lower.starts_with("enchanted creature")
        || lower.starts_with("enchanted permanent")
        || lower.starts_with("equipped creature")
    {
        let (filter, _) = parse_target(subject);
        return Some(SubjectApplication {
            affected: filter,
            target: None,
        });
    }
    // Bare plural noun phrase subjects ("creatures you control", "other creatures you control")
    // are implicit "all X" forms — strip any "other " prefix and route through parse_target.
    let noun_subject = lower.strip_prefix("other ").unwrap_or(&lower);
    if !noun_subject.starts_with("target ")
        && !noun_subject.starts_with("all ")
        && !noun_subject.starts_with("each ")
    {
        let normalized = format!("all {noun_subject}");
        let (filter, rest) = parse_target(&normalized);
        if rest.trim().is_empty() {
            return subject_filter_application(filter, false);
        }
    }
    if lower == "that player" {
        return Some(SubjectApplication {
            affected: TargetFilter::Player,
            target: None,
        });
    }
    // CR 506.3d: "defending player" as subject — resolves from combat state.
    if lower == "defending player" {
        return Some(SubjectApplication {
            affected: TargetFilter::DefendingPlayer,
            target: None,
        });
    }
    if lower == "that controller" {
        return Some(SubjectApplication {
            affected: TargetFilter::Controller,
            target: None,
        });
    }
    if matches!(
        lower.as_str(),
        "~" | "this"
            | "it"
            | "this card"
            | "this creature"
            | "this permanent"
            | "this artifact"
            | "this land"
    ) {
        return Some(SubjectApplication {
            affected: TargetFilter::SelfRef,
            target: None,
        });
    }

    let (filter, rest) = parse_type_phrase(subject);
    if rest.trim().is_empty() {
        return subject_filter_application(filter, false);
    }

    None
}

fn subject_filter_application(filter: TargetFilter, targeted: bool) -> Option<SubjectApplication> {
    Some(SubjectApplication {
        target: targeted.then_some(filter.clone()),
        affected: filter,
    })
}

/// Build a Pump or PumpAll effect from a subject application and P/T values.
fn build_pump_effect(
    application: &SubjectApplication,
    power: PtValue,
    toughness: PtValue,
) -> Effect {
    if let Some(target) = application.target.clone() {
        Effect::Pump {
            power,
            toughness,
            target,
        }
    } else if application.affected == TargetFilter::SelfRef {
        Effect::Pump {
            power,
            toughness,
            target: TargetFilter::SelfRef,
        }
    } else {
        Effect::PumpAll {
            power,
            toughness,
            target: application.affected.clone(),
        }
    }
}

/// Split compound predicates like "get +1/+1 until end of turn and you gain 1 life"
/// into a pump clause with the remainder chained as a sub_ability.
fn try_split_pump_compound(
    normalized: &str,
    application: &SubjectApplication,
) -> Option<ParsedEffectClause> {
    let lower = normalized.to_lowercase();
    // Find " and " that separates two independent clauses after a pump+duration.
    let and_pos = lower.find(" and ")?;
    let pump_part = &normalized[..and_pos];
    let remainder = normalized[and_pos + " and ".len()..].trim();
    let (remainder_without_duration, _) = super::strip_trailing_duration(remainder);

    if !parse_continuous_modifications(remainder_without_duration).is_empty() {
        return None;
    }

    let (power, toughness, duration) = super::parse_pump_clause(pump_part)?;
    let effect = build_pump_effect(application, power, toughness);

    // Parse the remainder as an independent effect chain (sub_ability).
    let sub_ability = if remainder.is_empty() {
        None
    } else {
        Some(Box::new(super::parse_effect_chain(
            remainder,
            AbilityKind::Spell,
        )))
    };
    Some(ParsedEffectClause {
        effect,
        duration,
        sub_ability,
    })
}

fn build_continuous_clause(
    application: SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);

    // Try the full predicate first (simple pump with no compound).
    if let Some((power, toughness, duration)) = super::parse_pump_clause(&normalized) {
        let effect = build_pump_effect(&application, power, toughness);
        return Some(ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
        });
    }

    // Compound: "get +1/+1 until end of turn and you gain 1 life"
    // Split on " and " that follows a duration marker, producing a pump
    // with a chained sub_ability for the remainder.
    if let Some(clause) = try_split_pump_compound(&normalized, &application) {
        return Some(clause);
    }

    let (predicate, duration) = super::strip_trailing_duration(&normalized);
    let modifications = parse_continuous_modifications(predicate);
    if modifications.is_empty() {
        return None;
    }

    if let Some((power, toughness)) = extract_pump_modifiers(&modifications) {
        let effect = build_pump_effect(&application, power, toughness);
        return Some(ParsedEffectClause {
            effect,
            duration,
            sub_ability: None,
        });
    }

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(application.affected)
                .modifications(modifications)
                .description(predicate.to_string())],
            duration: duration.clone(),
            target: application.target,
        },
        duration,
        sub_ability: None,
    })
}

fn build_become_clause(
    application: SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);
    let (predicate, duration) = super::strip_trailing_duration(&normalized);
    // CR 722: "become the monarch" — special keyword action, not an animation.
    if predicate.strip_prefix("become ")?.trim() == "the monarch" {
        return Some(super::parsed_clause(Effect::BecomeMonarch));
    }
    // CR 611.2b: "Becomes" effects without explicit duration are permanent
    let duration = duration.or(Some(Duration::Permanent));
    let become_text = predicate.strip_prefix("become ")?.trim();

    // CR 205.3 / CR 305.7: "become the [type] of your choice" — player chooses a subtype.
    // Must intercept before parse_animation_spec which rejects "of your choice" patterns.
    if let Some(clause) = try_parse_become_choice(become_text, &application, duration.clone()) {
        return Some(clause);
    }

    let animation = parse_animation_spec(become_text)?;
    let modifications = animation_modifications(&animation);
    if modifications.is_empty() {
        return None;
    }

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(application.affected)
                .modifications(modifications)
                .description(predicate.to_string())],
            duration: duration.clone(),
            target: application.target,
        },
        duration,
        sub_ability: None,
    })
}

/// CR 205.3 / CR 305.7: Parse "become the creature type of your choice" and similar
/// patterns into a Choose → GenericEffect(AddChosenSubtype) chain.
fn try_parse_become_choice(
    become_text: &str,
    application: &SubjectApplication,
    duration: Option<Duration>,
) -> Option<ParsedEffectClause> {
    use crate::types::ability::{ChoiceType, ChosenSubtypeKind, ContinuousModification};

    let lower = become_text.to_lowercase();
    if !lower.ends_with("of your choice") {
        return None;
    }

    let (choice_type, modification) = if lower.contains("creature type") {
        (
            ChoiceType::CreatureType,
            ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::CreatureType,
            },
        )
    } else if lower.contains("basic land type") {
        (
            ChoiceType::BasicLandType,
            ContinuousModification::AddChosenSubtype {
                kind: ChosenSubtypeKind::BasicLandType,
            },
        )
    } else {
        // Color-based choices ("become the color of your choice") deferred —
        // the engine doesn't have SetChosenColor as a ContinuousModification yet.
        return None;
    };

    // Two-step: Choose (prompts player) → GenericEffect (applies chosen subtype).
    let apply_effect = Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(application.affected.clone())
            .modifications(vec![modification])
            .description(become_text.to_string())],
        duration: duration.clone(),
        target: application.target.clone(),
    };
    let sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        apply_effect,
    )));

    Some(ParsedEffectClause {
        effect: Effect::Choose {
            choice_type,
            persist: false,
        },
        duration,
        sub_ability,
    })
}

fn build_restriction_clause(
    application: SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);
    let (predicate, duration) = super::strip_trailing_duration(&normalized);
    let lower = predicate.to_lowercase();

    // CR 508.1d / CR 509.1a: Restriction predicates for attack/block/target.
    // Compound restrictions ("can't attack or block") produce multiple StaticDefinition entries.
    let modes = parse_restriction_modes(&lower)?;

    let static_abilities = modes
        .into_iter()
        .map(|mode| {
            StaticDefinition::new(mode)
                .affected(application.affected.clone())
                .description(predicate.to_string())
        })
        .collect();

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities,
            duration: duration.clone(),
            target: application.target,
        },
        duration,
        sub_ability: None,
    })
}

/// Parse restriction predicates into one or more `StaticMode` variants.
/// Handles simple ("can't block") and compound ("can't attack or block") patterns.
fn parse_restriction_modes(lower: &str) -> Option<Vec<StaticMode>> {
    // Simple restrictions
    if lower == "can't block" || lower == "cannot block" {
        return Some(vec![StaticMode::CantBlock]);
    }
    if lower == "can't attack" || lower == "cannot attack" {
        return Some(vec![StaticMode::CantAttack]);
    }
    if lower == "can't be blocked" || lower == "cannot be blocked" {
        return Some(vec![StaticMode::Other("CantBeBlocked".to_string())]);
    }
    // CR 508.1d + CR 509.1a: Compound "can't attack or block"
    if lower == "can't attack or block" || lower == "cannot attack or block" {
        return Some(vec![StaticMode::CantAttack, StaticMode::CantBlock]);
    }
    // CR 509.1a + "can't be blocked": Compound "can't block or be blocked"
    if lower == "can't block or be blocked" || lower == "cannot block or be blocked" {
        return Some(vec![
            StaticMode::CantBlock,
            StaticMode::Other("CantBeBlocked".to_string()),
        ]);
    }
    // CR 509.1c: "can't be blocked except by ..." — evasion restriction
    if lower.starts_with("can't be blocked except by ")
        || lower.starts_with("cannot be blocked except by ")
    {
        let except_text = lower
            .strip_prefix("can't be blocked except by ")
            .or_else(|| lower.strip_prefix("cannot be blocked except by "))
            .unwrap_or("");
        return Some(vec![StaticMode::Other(format!(
            "CantBeBlockedExceptBy:{}",
            except_text
        ))]);
    }
    // CR 115.4: "can't be the target of ..." — hexproof variant
    if lower.starts_with("can't be the target of ") || lower.starts_with("cannot be the target of ")
    {
        return Some(vec![StaticMode::CantBeTargeted]);
    }

    None
}

fn extract_pump_modifiers(
    modifications: &[crate::types::ability::ContinuousModification],
) -> Option<(PtValue, PtValue)> {
    let mut power = None;
    let mut toughness = None;

    for modification in modifications {
        match modification {
            crate::types::ability::ContinuousModification::AddPower { value } => {
                power = Some(PtValue::Fixed(*value));
            }
            crate::types::ability::ContinuousModification::AddToughness { value } => {
                toughness = Some(PtValue::Fixed(*value));
            }
            _ => return None,
        }
    }

    Some((power?, toughness?))
}

/// Detect "its controller gains life equal to its power" and similar patterns where
/// the targeted permanent's controller gains life based on the permanent's stats.
pub(super) fn try_parse_targeted_controller_gain_life(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    if !lower.starts_with("its controller ") {
        return None;
    }
    if !lower.contains("gain") || !lower.contains("life") {
        return None;
    }
    let amount = if lower.contains("equal to its power") || lower.contains("its power") {
        QuantityExpr::Ref {
            qty: QuantityRef::TargetPower,
        }
    } else {
        // Try to parse a fixed amount: "its controller gains 3 life"
        let after = &lower["its controller ".len()..];
        let after = after
            .strip_prefix("gains ")
            .or_else(|| after.strip_prefix("gain "))
            .unwrap_or(after);
        QuantityExpr::Fixed {
            value: parse_number(after).map(|(n, _)| n as i32).unwrap_or(1),
        }
    };
    Some(parsed_clause(Effect::GainLife {
        amount,
        player: GainLifePlayer::TargetedController,
    }))
}

fn strip_subject_clause(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if !starts_with_subject_prefix(&lower) {
        return None;
    }

    let verb_start = find_predicate_start(text)?;
    let predicate = text[verb_start..].trim();
    if predicate.is_empty() {
        return None;
    }

    Some(deconjugate_verb(predicate))
}

/// Strip third-person 's' from the first word: "discards a card" → "discard a card".
pub(super) fn deconjugate_verb(text: &str) -> String {
    let text = text.trim();
    let first_space = text.find(' ').unwrap_or(text.len());
    let verb = &text[..first_space];
    let rest = &text[first_space..];
    let base = super::normalize_verb_token(verb);
    format!("{}{}", base, rest)
}

pub(super) fn starts_with_subject_prefix(lower: &str) -> bool {
    [
        "all ",
        "defending player ",
        "each opponent ",
        "each player ",
        "enchanted ",
        "equipped ",
        "it ",
        "its controller ",
        "target ",
        "that ",
        "the chosen ",
        "they ",
        "this ",
        "those ",
        "up to ",
        "you ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

pub(super) fn find_predicate_start(text: &str) -> Option<usize> {
    const VERBS: &[&str] = &[
        "add",
        "attack",
        "become",
        "block",
        "can",
        "cast",
        "choose",
        "connive",
        "copy",
        "counter",
        "create",
        "deal",
        "discard",
        "draw",
        "exile",
        "explore",
        "fight",
        "gain",
        "get",
        "have",
        "look",
        "lose",
        "mill",
        "pay",
        "phase",
        "put",
        "regenerate",
        "reveal",
        "return",
        "sacrifice",
        "scry",
        "search",
        "shuffle",
        "surveil",
        "tap",
        "transform",
        "untap",
    ];

    let lower = text.to_lowercase();
    let mut word_start = None;

    for (idx, ch) in lower.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = word_start.take() {
                let token = &lower[start..idx];
                if VERBS.contains(&super::normalize_verb_token(token).as_str()) {
                    return Some(start);
                }
            }
            continue;
        }

        if word_start.is_none() {
            word_start = Some(idx);
        }
    }

    if let Some(start) = word_start {
        let token = &lower[start..];
        if VERBS.contains(&super::normalize_verb_token(token).as_str()) {
            return Some(start);
        }
    }

    None
}
