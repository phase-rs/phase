use super::animation::{animation_modifications, parse_animation_spec};
use super::types::*;
use super::{resolve_it_pronoun, ParseContext};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Duration, Effect,
    FilterProp, GainLifePlayer, PtValue, QuantityExpr, QuantityRef, StaticDefinition, TargetFilter,
    TypedFilter,
};
use crate::types::statics::StaticMode;

use super::super::oracle_static::parse_continuous_modifications;
use super::super::oracle_target::{parse_target, parse_type_phrase};
use super::super::oracle_util::{parse_number, TextPair, SELF_REF_TYPE_PHRASES};

pub(super) fn try_parse_subject_predicate_ast(text: &str, ctx: &ParseContext) -> Option<ClauseAst> {
    if try_parse_targeted_controller_gain_life(text).is_some() {
        return None;
    }

    if let Some(clause) = try_parse_subject_continuous_clause(text, ctx) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, sub_ability| PredicateAst::Continuous {
                effect,
                duration,
                sub_ability,
            },
            ctx,
        ));
    }

    if let Some(clause) = try_parse_subject_become_clause(text, ctx) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, sub_ability| PredicateAst::Become {
                effect,
                duration,
                sub_ability,
            },
            ctx,
        ));
    }

    if let Some(clause) = try_parse_subject_restriction_clause(text, ctx) {
        return Some(subject_predicate_ast_from_clause(
            text,
            clause,
            |effect, duration, _sub_ability| PredicateAst::Restriction { effect, duration },
            ctx,
        ));
    }

    if let Some(stripped) = strip_subject_clause(text) {
        let subject_text = extract_subject_text(text)?;
        let application =
            parse_subject_application(&subject_text, ctx).unwrap_or(SubjectApplication {
                affected: TargetFilter::Any,
                target: None,
            });
        return Some(ClauseAst::SubjectPredicate {
            subject: Box::new(SubjectPhraseAst {
                affected: application.affected,
                target: application.target,
            }),
            predicate: Box::new(PredicateAst::ImperativeFallback { text: stripped }),
        });
    }

    None
}

fn subject_predicate_ast_from_clause<F>(
    text: &str,
    clause: ParsedEffectClause,
    build_predicate: F,
    ctx: &ParseContext,
) -> ClauseAst
where
    F: FnOnce(Effect, Option<Duration>, Option<Box<AbilityDefinition>>) -> PredicateAst,
{
    let subject_text = extract_subject_text(text).unwrap_or_default();
    let application = parse_subject_application(&subject_text, ctx).unwrap_or(SubjectApplication {
        affected: TargetFilter::Any,
        target: None,
    });

    ClauseAst::SubjectPredicate {
        subject: Box::new(SubjectPhraseAst {
            affected: application.affected,
            target: application.target,
        }),
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

fn try_parse_subject_continuous_clause(
    text: &str,
    ctx: &ParseContext,
) -> Option<ParsedEffectClause> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = text[verb_start..].trim();
    let application = parse_subject_application(subject, ctx)?;
    build_continuous_clause(application, predicate)
}

fn try_parse_subject_become_clause(text: &str, ctx: &ParseContext) -> Option<ParsedEffectClause> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = deconjugate_verb(text[verb_start..].trim());
    if !predicate.to_lowercase().starts_with("become ") {
        return None;
    }
    let application = parse_subject_application(subject, ctx)?;
    build_become_clause(application, &predicate)
}

fn try_parse_subject_restriction_clause(
    text: &str,
    ctx: &ParseContext,
) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();

    // CR 509.1c: "Target creature must be blocked [this turn] [if able]"
    // Handled separately because "must be blocked" isn't a "can't X" restriction pattern
    // and needs AddStaticMode for transient effect propagation through the layer system.
    let tp = TextPair::new(text, &lower);
    if let Some((before, _)) = tp.split_around(" must be blocked") {
        let subject = before.original.trim();
        let application = parse_subject_application(subject, ctx)?;
        return Some(ParsedEffectClause {
            effect: Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::new(StaticMode::MustBeBlocked)
                    .affected(application.affected)
                    .modifications(vec![ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustBeBlocked,
                    }])],
                duration: Some(Duration::UntilEndOfTurn),
                target: application.target,
            },
            distribute: None,
            multi_target: None,
            duration: Some(Duration::UntilEndOfTurn),
            sub_ability: None,
        });
    }

    let (subject, predicate) = if let Some(pos) = tp.find(" can't ") {
        let (before, after) = tp.split_at(pos);
        (before.original.trim(), after.original[1..].trim())
    } else if let Some(pos) = tp.find(" cannot ") {
        let (before, after) = tp.split_at(pos);
        (before.original.trim(), after.original[1..].trim())
    } else if let Some(pos) = tp.find(" doesn't untap") {
        // CR 302.6: "doesn't untap during [controller's] untap step"
        let (before, after) = tp.split_at(pos);
        (before.original.trim(), after.original[1..].trim())
    } else if let Some(pos) = tp.find(" don't untap") {
        let (before, after) = tp.split_at(pos);
        (before.original.trim(), after.original[1..].trim())
    } else {
        return None;
    };
    let application = parse_subject_application(subject, ctx)?;
    build_restriction_clause(application, predicate)
}

fn parse_subject_application(subject: &str, ctx: &ParseContext) -> Option<SubjectApplication> {
    if subject.trim().is_empty() {
        return None;
    }

    let lower = subject.to_lowercase();

    // CR 115.10a: "another target X" — target with Another filter property,
    // excluding the source object from legal targets.
    if lower.starts_with("another target ") {
        let (filter, _) = parse_target(&subject["another ".len()..]);
        let filter = add_another_property(filter);
        return subject_filter_application(filter, true);
    }
    if lower.starts_with("target ") {
        let (filter, _) = parse_target(subject);
        return subject_filter_application(filter, true);
    }
    // "up to N target X" — the quantifier count is handled by strip_any_number_quantifier()
    // in parse_effect_chain; here we just need to route to parse_target for the filter.
    if lower.starts_with("up to ") {
        let tp = TextPair::new(subject, &lower);
        if let Some(pos) = tp.find("target ") {
            let (_, from_target) = tp.split_at(pos);
            let (filter, _) = parse_target(from_target.original);
            return subject_filter_application(filter, true);
        }
    }
    // "each of your opponents" / "each of those creatures" / "each of them" — variant of
    // "each" with an interposed "of" that parse_target doesn't handle directly.
    // Must check before "each " to avoid the generic "each" path swallowing "each of".
    if let Some(remainder) = lower.strip_prefix("each of ") {
        if remainder.starts_with("your opponents") || remainder.starts_with("your opponent") {
            return subject_filter_application(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                false,
            );
        }
        // "each of those [creatures/players/...]" / "each of them" — anaphoric reference
        // to the targets declared in the parent ability's sub_ability chain.
        if remainder.starts_with("those ") || remainder.starts_with("them") {
            return subject_filter_application(TargetFilter::ParentTarget, false);
        }
        // Fallback: strip "of " and re-route through parse_target as "each <remainder>"
        let normalized = format!("each {remainder}");
        let (filter, _) = parse_target(&normalized);
        return subject_filter_application(filter, false);
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
    let (had_other, noun_subject) = if let Some(rest) = lower.strip_prefix("other ") {
        (true, rest)
    } else {
        (false, lower.as_str())
    };
    if !noun_subject.starts_with("target ")
        && !noun_subject.starts_with("all ")
        && !noun_subject.starts_with("each ")
    {
        let normalized = format!("all {noun_subject}");
        let (filter, rest) = parse_target(&normalized);
        if rest.trim().is_empty() {
            let filter = if had_other {
                add_another_property(filter)
            } else {
                filter
            };
            return subject_filter_application(filter, false);
        }
    }
    // CR 119.7: "players" as bare plural subject (e.g., "players can't gain life")
    if lower == "players" {
        return Some(SubjectApplication {
            affected: TargetFilter::Typed(TypedFilter::default()),
            target: None,
        });
    }
    if lower == "that player" || lower == "the player" {
        return Some(SubjectApplication {
            affected: TargetFilter::Player,
            target: None,
        });
    }
    // "an opponent" as subject — single opponent (two-player: equivalent to "each opponent").
    if lower.starts_with("an opponent") {
        return subject_filter_application(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            false,
        );
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
    // Explicit self-reference — always SelfRef
    if matches!(lower.as_str(), "~" | "this" | "this card")
        || SELF_REF_TYPE_PHRASES.iter().any(|p| lower == *p)
    {
        return Some(SubjectApplication {
            affected: TargetFilter::SelfRef,
            target: None,
        });
    }
    // CR 608.2k: Bare pronoun "it" — context-dependent
    if lower == "it" {
        return Some(SubjectApplication {
            affected: resolve_it_pronoun(ctx),
            target: None,
        });
    }

    // CR 608.2c: "that creature/permanent/land" — anaphoric back-reference to a
    // previously mentioned object in the same effect sequence. Strip "that " and parse
    // the remainder as a type phrase. Covers all "that [type]" patterns generically.
    if let Some(rest_subject) = lower.strip_prefix("that ") {
        // CR 608.2c: "that creature/permanent/land" — anaphoric back-reference to a
        // previously mentioned object in the same effect sequence. Strip "that " and parse
        // the remainder as a type phrase. Covers all "that [type]" patterns generically.
        let original_rest = &subject[subject.len() - rest_subject.len()..];
        let (filter, rem) = parse_type_phrase(original_rest);
        if rem.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
            return Some(SubjectApplication {
                affected: filter,
                target: None,
            });
        }
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
    let tp = TextPair::new(normalized, &lower);
    let (pump_tp, remainder_tp) = tp.split_around(" and ")?;
    let pump_part = pump_tp.original;
    let remainder = remainder_tp.original.trim();
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
        distribute: None,
        multi_target: None,
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
            distribute: None,
            multi_target: None,
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
            distribute: None,
            multi_target: None,
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
        distribute: None,
        multi_target: None,
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

    // CR 707.2 / CR 613.1a: "become a copy of [target]" — copy copiable characteristics.
    // Must intercept before parse_animation_spec which rejects "copy of" patterns.
    const COPY_PREFIX: &str = "a copy of ";
    if let Some(copy_target_text) = become_text
        .get(..COPY_PREFIX.len())
        .filter(|s| s.eq_ignore_ascii_case(COPY_PREFIX))
        .map(|_| &become_text[COPY_PREFIX.len()..])
    {
        let (target, _) = parse_target(copy_target_text);
        return Some(ParsedEffectClause {
            effect: Effect::BecomeCopy {
                target,
                duration: duration.clone(),
            },
            duration,
            sub_ability: None,
            distribute: None,
            multi_target: None,
        });
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
        distribute: None,
        multi_target: None,
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
    } else if lower.contains("color") {
        // CR 105.3: "become the color of your choice" — player chooses a color.
        (ChoiceType::Color, ContinuousModification::AddChosenColor)
    } else {
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
        distribute: None,
        multi_target: None,
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
        distribute: None,
        multi_target: None,
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
        return Some(vec![StaticMode::CantBeBlocked]);
    }
    // CR 508.1d + CR 509.1a: Compound "can't attack or block"
    if lower == "can't attack or block" || lower == "cannot attack or block" {
        return Some(vec![StaticMode::CantAttack, StaticMode::CantBlock]);
    }
    // CR 509.1a + "can't be blocked": Compound "can't block or be blocked"
    if lower == "can't block or be blocked" || lower == "cannot block or be blocked" {
        return Some(vec![StaticMode::CantBlock, StaticMode::CantBeBlocked]);
    }
    // CR 509.1c: "can't be blocked except by ..." — evasion restriction
    if lower.starts_with("can't be blocked except by ")
        || lower.starts_with("cannot be blocked except by ")
    {
        let except_text = lower
            .strip_prefix("can't be blocked except by ")
            .or_else(|| lower.strip_prefix("cannot be blocked except by "))
            .unwrap_or("");
        return Some(vec![StaticMode::CantBeBlockedExceptBy {
            filter: except_text.to_string(),
        }]);
    }
    // CR 115.4: "can't be the target of ..." — hexproof variant
    if lower.starts_with("can't be the target of ") || lower.starts_with("cannot be the target of ")
    {
        return Some(vec![StaticMode::CantBeTargeted]);
    }
    // CR 119.7: "can't gain life" — lifegain prevention
    if lower == "can't gain life" || lower == "cannot gain life" {
        return Some(vec![StaticMode::CantGainLife]);
    }
    // CR 302.6: "doesn't untap during [controller's] untap step"
    if lower.starts_with("doesn't untap") || lower.starts_with("don't untap") {
        return Some(vec![StaticMode::CantUntap]);
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

pub(crate) fn starts_with_subject_prefix(lower: &str) -> bool {
    [
        "all ",
        "an opponent ",
        "defending player ",
        "each of ",
        "each opponent ",
        "each player ",
        "enchanted ",
        "equipped ",
        "it ",
        "its controller ",
        "target ",
        "that ",
        "the chosen ",
        "the player ",
        "they ",
        "this ",
        "those ",
        "up to ",
        "you ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

/// Verbs recognized for subject-predicate splitting in Oracle text.
/// Also used by `gap_analysis` to classify unimplemented effect text.
pub(crate) const PREDICATE_VERBS: &[&str] = &[
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
    "win",
];

pub(super) fn find_predicate_start(text: &str) -> Option<usize> {
    let lower = text.to_lowercase();
    let mut word_start = None;

    for (idx, ch) in lower.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = word_start.take() {
                let token = &lower[start..idx];
                if PREDICATE_VERBS.contains(&super::normalize_verb_token(token).as_str()) {
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
        if PREDICATE_VERBS.contains(&super::normalize_verb_token(token).as_str()) {
            return Some(start);
        }
    }

    None
}

/// Add `FilterProp::Another` to a target filter, ensuring the source is excluded.
fn add_another_property(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            if !tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::Another))
            {
                tf.properties.push(FilterProp::Another);
            }
            TargetFilter::Typed(tf)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::TypeFilter;

    #[test]
    fn starts_with_subject_prefix_each_of() {
        assert!(starts_with_subject_prefix("each of your opponents"));
        assert!(starts_with_subject_prefix("each of those creatures"));
        assert!(starts_with_subject_prefix("each of them"));
    }

    #[test]
    fn starts_with_subject_prefix_an_opponent() {
        assert!(starts_with_subject_prefix("an opponent discards a card"));
        assert!(starts_with_subject_prefix(
            "an opponent sacrifices a creature"
        ));
    }

    #[test]
    fn starts_with_subject_prefix_the_player() {
        assert!(starts_with_subject_prefix("the player draws a card"));
    }

    #[test]
    fn parse_subject_each_of_your_opponents() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("each of your opponents", &ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert_eq!(
            app.affected,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
        assert!(
            app.target.is_none(),
            "each of your opponents is non-targeted"
        );
    }

    #[test]
    fn parse_subject_each_of_them() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("each of them", &ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert_eq!(app.affected, TargetFilter::ParentTarget);
    }

    #[test]
    fn parse_subject_each_of_those_creatures() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("each of those creatures", &ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert_eq!(app.affected, TargetFilter::ParentTarget);
    }

    #[test]
    fn parse_subject_an_opponent() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("an opponent", &ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert_eq!(
            app.affected,
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        );
    }

    #[test]
    fn parse_subject_the_player() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("the player", &ctx);
        assert!(result.is_some());
        let app = result.unwrap();
        assert_eq!(app.affected, TargetFilter::Player);
    }

    // CR 608.2c: "that [type]" anaphoric back-references
    #[test]
    fn parse_subject_that_creature() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("That creature", &ctx);
        assert!(result.is_some(), "should recognize 'That creature'");
        let app = result.unwrap();
        assert!(
            matches!(app.affected, TargetFilter::Typed(ref t) if t.type_filters.contains(&TypeFilter::Creature)),
            "affected should be Creature filter, got {:?}",
            app.affected
        );
        assert!(app.target.is_none(), "anaphoric ref is non-targeted");
    }

    #[test]
    fn parse_subject_that_land() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("that land", &ctx);
        assert!(result.is_some(), "should recognize 'that land'");
        let app = result.unwrap();
        assert!(
            matches!(app.affected, TargetFilter::Typed(ref t) if t.type_filters.contains(&TypeFilter::Land)),
            "affected should be Land filter, got {:?}",
            app.affected
        );
    }

    #[test]
    fn parse_subject_that_permanent() {
        let ctx = ParseContext::default();
        let result = parse_subject_application("that permanent", &ctx);
        assert!(result.is_some(), "should recognize 'that permanent'");
        let app = result.unwrap();
        assert!(
            matches!(app.affected, TargetFilter::Typed(ref t) if t.type_filters.contains(&TypeFilter::Permanent)),
            "affected should be Permanent filter, got {:?}",
            app.affected
        );
    }

    #[test]
    fn parse_subject_that_player_unchanged() {
        // "that player" has its own handler at line 266 — ensure "that " prefix
        // doesn't shadow it (it shouldn't, since it's checked earlier)
        let ctx = ParseContext::default();
        let result = parse_subject_application("that player", &ctx);
        assert!(result.is_some());
        assert_eq!(result.unwrap().affected, TargetFilter::Player);
    }
}
